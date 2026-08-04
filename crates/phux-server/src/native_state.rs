//! Native libghostty checkpoint capability probing and bounded state hosts.
//!
//! authenticated READY record. Retained history is a separate, client-pulled
//! stream backed by one cursor-keyed immutable record cache per checkpoint
//! generation. Records and tokens remain opaque.

use bytes::Bytes;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use libghostty_vt::{
    Terminal as GhosttyTerminal,
    snapshot::incremental::{
        self, Capture, CaptureContinuation, CaptureEventKind, CaptureOptions, ContinuationOptions,
        DetachOptions, HistoryEvent, HistoryOptions, LiveHistoryCursor, ScreenKey, TOKEN_LEN,
    },
};
use phux_protocol::caps::{BootstrapCapabilities, BootstrapLimits, EngineCodec, EngineFeatureSet};

const INCREMENTAL_ABI_VERSION: u32 = 1;
const CHECKPOINT_VERSION: u16 = EngineCodec::LibghosttyCheckpointV2 as u16;
const CHECKPOINT_CODEC_IDENTITY: &str = "ghostty.snapshot.v1-v2.incremental.v1";

/// Greatest number of opaque codec records retained before READY publication.
pub(crate) const MAX_NATIVE_PREFIX_CHUNKS: usize = 4_096;
/// Greatest aggregate opaque codec payload retained before READY publication.
pub(crate) const MAX_NATIVE_PREFIX_BYTES: usize = 64 * 1024 * 1024;

/// Exact status reported by libghostty's incremental checkpoint API.
///
/// This is a public rename, not a translated error. Callers can match
/// `UnsupportedFeature`, `OutOfSpace`, `LimitExceeded`, `OutOfMemory`, `Stale`,
/// `Reset`, `Resize`, and every other native status without parsing a string.
pub use incremental::Error as NativeStateError;

/// One authenticated engine token carried opaquely by protocol 0.7.
///
/// Phux may compare or route these bytes as an identity but never parses,
/// normalizes, or reconstructs the native token.
pub type OpaqueHistoryCursor = [u8; TOKEN_LEN];

/// Probe the linked engine and advertise native checkpoint v2 only when its
/// complete protocol-0.7 contract is present.
///
/// A failed or incomplete probe leaves both synthesized VT profiles available
/// and advertises no native codec or partial native feature set.
#[must_use]
pub fn native_bootstrap_capabilities() -> BootstrapCapabilities {
    let requested = BootstrapLimits::default();
    let synthesized = BootstrapCapabilities::new().with_limits(requested);
    let Ok(engine) = incremental::capabilities() else {
        return synthesized;
    };
    if !supports_protocol_07_native(&engine) {
        return synthesized;
    }
    let Some(limits) = intersect_engine_limits(requested, &engine) else {
        return synthesized;
    };

    BootstrapCapabilities::new()
        .with_limits(limits)
        .with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        )
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
    /// Exact native envelope version emitted by the engine.
    pub codec_version: u16,
    /// Complete opaque envelope or record bytes.
    pub bytes: &'buffer [u8],
}

fn bounded_capture_options(
    _limits: BootstrapLimits,
    engine: &incremental::Capabilities,
) -> Result<CaptureOptions, NativeStateError> {
    let max_record_bytes = engine.max_record_bytes.min(MAX_NATIVE_PREFIX_BYTES);
    if max_record_bytes == 0 {
        return Err(NativeStateError::LimitExceeded);
    }
    let max_pages = CaptureOptions::default().max_pages.min(engine.max_pages);
    if max_pages == 0 {
        return Err(NativeStateError::LimitExceeded);
    }
    Ok(CaptureOptions {
        max_record_bytes,
        max_pages,
    })
}

fn preflight_checkpoint_prefix(
    terminal: &mut GhosttyTerminal<'_, '_>,
    options: CaptureOptions,
    max_record_bytes: usize,
    max_prefix_bytes: usize,
    max_prefix_chunks: usize,
    wire_chunk_bytes: usize,
) -> Result<(), NativeStateError> {
    let mut capture = terminal.capture(options)?;
    let mut buffer = Vec::new();
    let mut prefix_bytes = 0_usize;
    let mut prefix_chunks = 0_usize;
    loop {
        match capture.next(&mut buffer) {
            Err(NativeStateError::OutOfSpace {
                required_bytes,
                required_rows: 0,
            }) if required_bytes != 0 && required_bytes <= max_record_bytes => {
                buffer
                    .try_reserve(required_bytes.saturating_sub(buffer.len()))
                    .map_err(|_| NativeStateError::OutOfMemory)?;
                buffer.resize(required_bytes, 0);
            }
            Err(error) => return Err(error),
            Ok(event) => {
                prefix_bytes = prefix_bytes
                    .checked_add(event.record.len())
                    .filter(|bytes| *bytes <= max_prefix_bytes)
                    .ok_or(NativeStateError::LimitExceeded)?;
                let event_chunks = event.record.len().div_ceil(wire_chunk_bytes);
                prefix_chunks = prefix_chunks
                    .checked_add(event_chunks)
                    .filter(|chunks| *chunks <= max_prefix_chunks)
                    .ok_or(NativeStateError::LimitExceeded)?;
                if event.codec_version != CHECKPOINT_VERSION
                    || event.record.len() > max_record_bytes
                {
                    return Err(NativeStateError::LimitExceeded);
                }
                match event.kind {
                    CaptureEventKind::Record => {}
                    CaptureEventKind::Ready { .. } => {
                        capture.abort()?;
                        return Ok(());
                    }
                    CaptureEventKind::HistoryBegin { .. }
                    | CaptureEventKind::HistoryPage { .. }
                    | CaptureEventKind::Finish => return Err(NativeStateError::InvalidState),
                }
            }
        }
    }
}

/// RAII host for the bounded checkpoint prefix ending at READY.
///
/// Construction mutably borrows the canonical terminal and asks libghostty to
/// validate all terminal-owned semantic state before returning. A caller may
/// therefore publish `BOOTSTRAP_BEGIN` only after `new` succeeds. Once `step`
/// returns READY, no history or FINISH record can be emitted by this host.
#[derive(Debug)]
pub struct NativeCheckpointCapture<'terminal> {
    capture: Option<Capture<'terminal, 'static>>,
    max_record_bytes: usize,
    ready: bool,
}

impl<'terminal> NativeCheckpointCapture<'terminal> {
    /// Preflight and begin a checkpoint using negotiated payload limits.
    ///
    /// No output is emitted during construction. Kitty graphics, glyph
    /// glossary state, continuation failures, limits, and OOM all fail before
    /// the caller can publish BEGIN.
    pub fn new(
        terminal: &'terminal mut GhosttyTerminal<'_, '_>,
        limits: BootstrapLimits,
    ) -> Result<Self, NativeStateError> {
        let engine = require_protocol_07_native()?;
        let options = bounded_capture_options(limits, &engine)?;
        let max_record_bytes = options.max_record_bytes;
        preflight_checkpoint_prefix(
            terminal,
            options,
            max_record_bytes,
            MAX_NATIVE_PREFIX_BYTES,
            MAX_NATIVE_PREFIX_CHUNKS,
            max_record_bytes,
        )?;
        let capture = terminal.capture(options)?;
        Ok(Self {
            capture: Some(capture),
            max_record_bytes,
            ready: false,
        })
    }

    /// Emit one complete opaque prefix record directly into `buffer`.
    ///
    /// A zero or short buffer returns libghostty's exact `OutOfSpace` status,
    /// including `required_bytes`, without advancing. READY is returned exactly
    /// once and exhausts this bootstrap host; later calls return `InvalidState`
    /// rather than leaking native HISTORY or FINISH records into bootstrap.
    pub fn step<'buffer>(
        &mut self,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeCheckpointChunk<'buffer>, NativeStateError> {
        if self.ready {
            return Err(NativeStateError::InvalidState);
        }
        let capture = self
            .capture
            .as_mut()
            .ok_or(NativeStateError::InvalidState)?;
        let event = capture.next(buffer)?;
        if event.codec_version != CHECKPOINT_VERSION {
            return Err(NativeStateError::UnknownVersion);
        }
        if event.record.len() > self.max_record_bytes {
            return Err(NativeStateError::LimitExceeded);
        }

        let kind = match event.kind {
            CaptureEventKind::Record => NativeCheckpointChunkKind::Record,
            CaptureEventKind::Ready { .. } => {
                self.ready = true;
                NativeCheckpointChunkKind::Ready
            }
            CaptureEventKind::HistoryBegin { .. }
            | CaptureEventKind::HistoryPage { .. }
            | CaptureEventKind::Finish => return Err(NativeStateError::InvalidState),
        };
        Ok(NativeCheckpointChunk {
            kind,
            codec_version: event.codec_version,
            bytes: event.record,
        })
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

    /// Abort explicitly, releasing the terminal borrow and native resources.
    ///
    /// The caller performs this immediately after READY, then transfers the
    /// unchanged terminal into [`NativeHistoryCursor`] before publishing READY.
    pub fn abort(mut self) -> Result<(), NativeStateError> {
        self.capture
            .take()
            .ok_or(NativeStateError::InvalidState)?
            .abort()
    }
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

/// Owned canonical terminal plus its live, newest-first history cursor.
///
/// The engine's copy-on-write cut keeps older history stable while
/// [`Self::vt_write`] accepts serialized live PTY output. Reset, resize,
/// pruning, stale generations, bounds, and OOM remain exact typed engine errors.
#[derive(Debug)]
pub struct NativeHistoryCursor<'terminal_alloc, 'cb> {
    inner: LiveHistoryCursor<'terminal_alloc, 'cb, 'static>,
    max_unit_bytes: usize,
    max_rows: usize,
    max_units: usize,
}

impl<'terminal_alloc, 'cb> NativeHistoryCursor<'terminal_alloc, 'cb> {
    /// Consume the canonical terminal and acquire its primary retained-history
    /// cut before the caller publishes `BOOTSTRAP_READY`.
    pub fn new(
        terminal: GhosttyTerminal<'terminal_alloc, 'cb>,
        limits: BootstrapLimits,
    ) -> Result<Self, NativeStateError> {
        let engine = require_protocol_07_native()?;
        let max_unit_bytes = usize::try_from(limits.max_history_page_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?
            .min(engine.max_unit_bytes);
        let defaults = HistoryOptions::default();
        let max_rows = defaults.max_rows.min(engine.max_rows);
        let max_units = defaults.max_units.min(engine.max_pages);
        if max_unit_bytes == 0 || max_rows == 0 || max_units == 0 {
            return Err(NativeStateError::LimitExceeded);
        }

        let inner = terminal.into_live_history_cursor(ScreenKey::PRIMARY)?;
        Ok(Self {
            inner,
            max_unit_bytes,
            max_rows,
            max_units,
        })
    }

    /// Opaque checkpoint authenticating this cursor's exact terminal cut.
    #[must_use]
    pub fn checkpoint(&self) -> &OpaqueHistoryCursor {
        self.inner.checkpoint().as_bytes()
    }

    /// Opaque capability advertised as `BOOTSTRAP_READY.history_cursor`.
    #[must_use]
    pub fn cursor(&self) -> &OpaqueHistoryCursor {
        self.inner.capability().as_bytes()
    }

    /// Borrow the live canonical terminal for read-only engine queries.
    #[must_use]
    pub fn terminal(&self) -> &GhosttyTerminal<'terminal_alloc, 'cb> {
        self.inner.terminal()
    }

    /// Feed serialized raw PTY bytes to the live canonical terminal.
    pub fn vt_write(&mut self, data: &[u8]) {
        self.inner.vt_write(data);
    }

    /// Reset the live terminal and invalidate this retained-history generation.
    pub fn reset(&mut self) {
        self.inner.reset();
    }

    /// Resize the live terminal and invalidate this retained-history generation.
    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> libghostty_vt::error::Result<()> {
        self.inner.resize(cols, rows, cell_width_px, cell_height_px)
    }

    /// Emit one authenticated history unit within both negotiated bounds and
    /// the request's non-zero `max_bytes` bound.
    ///
    /// Short buffers return exact `OutOfSpace` without advancing. The returned
    /// cursor bytes are copied from the engine capability without parsing or
    /// rewriting them.
    pub fn next<'buffer>(
        &mut self,
        max_bytes: u32,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeHistoryEvent<'buffer>, NativeStateError> {
        let requested = usize::try_from(max_bytes).map_err(|_| NativeStateError::LimitExceeded)?;
        if requested == 0 {
            return Err(NativeStateError::LimitExceeded);
        }
        let options = HistoryOptions {
            max_unit_bytes: requested.min(self.max_unit_bytes),
            max_rows: self.max_rows,
            max_units: self.max_units,
        };
        match self.inner.next(options, buffer)? {
            HistoryEvent::Unit {
                unit,
                rows,
                page_complete,
            } => Ok(NativeHistoryEvent::Page {
                bytes: unit,
                rows,
                page_complete,
                next_cursor: *self.inner.capability().as_bytes(),
            }),
            HistoryEvent::End => Ok(NativeHistoryEvent::End),
        }
    }

    /// Release cursor and lease state before returning the live terminal.
    #[must_use]
    pub fn into_terminal(self) -> GhosttyTerminal<'terminal_alloc, 'cb> {
        self.inner.into_terminal()
    }
}

#[derive(Debug)]
pub(crate) struct NativeManagerInitFailure {
    pub(crate) error: NativeStateError,
    pub(crate) terminal: GhosttyTerminal<'static, 'static>,
}

/// Fixed bounds for one shared retained-history generation.
///
/// These values are captured when READY is detached. They never vary with a
/// particular owner's request, so the first owner to reach the frontier cannot
/// choose a different native page partition for later owners.
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
    /// Retained bytes needed for the exact fixed record table and complete
    /// engine/protocol native payload budget.
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
/// This type deliberately is neither clonable nor detached from the native
/// continuation's auto-traits. It must be installed back into the actor-local
/// manager rather than sent to another thread.
#[derive(Debug)]
pub(crate) struct NativeGenerationSeed {
    continuation: CaptureContinuation<'static>,
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

    const fn len(&self) -> usize {
        self.len
    }

    fn push(&mut self, record: CachedNativeHistoryRecord) {
        self.slots[self.len] = Some(record);
        self.len += 1;
    }
}

#[derive(Debug)]
struct NativeCheckpointGeneration {
    continuation: Option<CaptureContinuation<'static>>,
    records: NativeRecordTable,
    bounds: NativeGenerationBounds,
    /// Exact retained bytes owned by cached record payloads.
    cached_capacity: usize,
    /// Payload capacity left after reserving the fixed record table.
    payload_capacity: usize,
    owners: usize,
    charge: Arc<NativeGenerationCharge>,
}

/// Actor-owned terminal and bounded concurrent native history cuts.
#[derive(Debug)]
pub(crate) struct NativeTerminalManager {
    terminal: GhosttyTerminal<'static, 'static>,
    /// Cursor-qualified generations used by shared native capture fanout.
    generations: HashMap<OpaqueHistoryCursor, NativeCheckpointGeneration>,
    /// Released generations whose cached payloads still escape through `Bytes`.
    retired_generation_charges: Vec<Arc<NativeGenerationCharge>>,
    capacity: usize,
    engine: incremental::Capabilities,
    /// True while an actor-owned cooperative prefix capture has the logical
    /// exclusive mutation lease on `terminal`.
    capture_active: bool,
}

impl NativeTerminalManager {
    pub(crate) fn new(
        terminal: GhosttyTerminal<'static, 'static>,
        capacity: usize,
    ) -> Result<Self, NativeManagerInitFailure> {
        let engine = match require_protocol_07_native() {
            Ok(engine) => engine,
            Err(error) => return Err(NativeManagerInitFailure { error, terminal }),
        };
        if capacity == 0 {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::LimitExceeded,
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
            engine,
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
    pub(crate) fn capture(
        &mut self,
        limits: BootstrapLimits,
    ) -> Result<NativeManagedCapture<'_>, NativeStateError> {
        self.capture_bounded(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
    }

    #[cfg(test)]
    pub(crate) fn capture_bounded(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
    ) -> Result<NativeManagedCapture<'_>, NativeStateError> {
        self.capture_bounded_inner(limits, max_prefix_bytes, max_prefix_chunks, true)
    }

    #[cfg(test)]
    fn capture_bounded_inner(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
        require_available_slot: bool,
    ) -> Result<NativeManagedCapture<'_>, NativeStateError> {
        if require_available_slot && self.retained_generation_count()? >= self.capacity {
            return Err(NativeStateError::LimitExceeded);
        }
        let mut options = bounded_capture_options(limits, &self.engine)?;
        options.max_record_bytes = options.max_record_bytes.min(max_prefix_bytes);
        if options.max_record_bytes == 0 || max_prefix_chunks == 0 {
            return Err(NativeStateError::LimitExceeded);
        }
        let wire_chunk_bytes = usize::try_from(limits.max_chunk_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?;
        let max_record_bytes = options.max_record_bytes;
        let max_pages = options.max_pages;
        let protocol_max_unit_bytes = usize::try_from(phux_protocol::MAX_HISTORY_PAGE_BYTES)
            .map_err(|_| NativeStateError::LimitExceeded)?;
        let max_unit_bytes = protocol_max_unit_bytes.min(self.engine.max_unit_bytes);
        let detach_defaults = DetachOptions::default();
        let protocol_max_rows = usize::try_from(phux_protocol::MAX_HISTORY_PAGE_ROWS)
            .map_err(|_| NativeStateError::LimitExceeded)?;
        let max_rows = detach_defaults
            .max_rows
            .min(self.engine.max_rows)
            .min(protocol_max_rows);
        let engine_protocol_total_bytes = max_unit_bytes
            .checked_mul(max_pages)
            .ok_or(NativeStateError::LimitExceeded)?;
        let max_total_bytes = detach_defaults
            .max_total_bytes
            .min(engine_protocol_total_bytes);
        let max_records = max_pages
            .checked_add(2)
            .ok_or(NativeStateError::LimitExceeded)?;
        if max_record_bytes == 0
            || max_unit_bytes == 0
            || max_pages == 0
            || max_rows == 0
            || max_total_bytes == 0
            || max_records == 0
        {
            return Err(NativeStateError::LimitExceeded);
        }
        preflight_checkpoint_prefix(
            &mut self.terminal,
            options,
            max_record_bytes,
            max_prefix_bytes,
            max_prefix_chunks,
            wire_chunk_bytes,
        )?;
        let capture = self.terminal.capture(options)?;
        Ok(NativeManagedCapture {
            capture: Some(capture),
            max_record_bytes,
            detach_options: DetachOptions {
                max_pages,
                max_total_bytes,
                max_rows,
            },
            generation_bounds: NativeGenerationBounds {
                max_record_bytes: max_unit_bytes,
                max_rows,
                max_records,
                max_total_bytes,
            },
            ready_cursor: None,
        })
    }

    /// Begin the prefix half of one future shared generation.
    ///
    /// Generation captures may reach READY while every cache slot is occupied:
    /// the resulting content-derived cursor can still join an existing slot.
    /// Installation remains the sole admission point for genuinely new cursors.
    #[cfg(test)]
    pub(crate) fn capture_generation_bounded(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
    ) -> Result<NativeManagedCapture<'_>, NativeStateError> {
        self.capture_bounded_inner(limits, max_prefix_bytes, max_prefix_chunks, false)
    }

    /// Start a cooperative actor-owned prefix capture without the eager
    /// preflight walk. The actor must call exactly one of
    /// [`Self::finish_generation_capture`] or [`Self::abort_generation_capture`]
    /// before mutating or reading the terminal again.
    pub(crate) fn begin_generation_capture(
        &mut self,
        limits: BootstrapLimits,
        max_prefix_bytes: usize,
        max_prefix_chunks: usize,
    ) -> Result<NativeManagedCapture<'static>, NativeStateError> {
        if self.capture_active || max_prefix_chunks == 0 {
            return Err(NativeStateError::InvalidState);
        }
        let mut options = bounded_capture_options(limits, &self.engine)?;
        options.max_record_bytes = options.max_record_bytes.min(max_prefix_bytes);
        if options.max_record_bytes == 0 {
            return Err(NativeStateError::LimitExceeded);
        }
        let max_record_bytes = options.max_record_bytes;
        let max_pages = options.max_pages;
        let protocol_max_unit_bytes = usize::try_from(phux_protocol::MAX_HISTORY_PAGE_BYTES)
            .map_err(|_| NativeStateError::LimitExceeded)?;
        let max_unit_bytes = protocol_max_unit_bytes.min(self.engine.max_unit_bytes);
        let detach_defaults = DetachOptions::default();
        let protocol_max_rows = usize::try_from(phux_protocol::MAX_HISTORY_PAGE_ROWS)
            .map_err(|_| NativeStateError::LimitExceeded)?;
        let max_rows = detach_defaults
            .max_rows
            .min(self.engine.max_rows)
            .min(protocol_max_rows);
        let max_total_bytes = detach_defaults.max_total_bytes.min(
            max_unit_bytes
                .checked_mul(max_pages)
                .ok_or(NativeStateError::LimitExceeded)?,
        );
        let max_records = max_pages
            .checked_add(2)
            .ok_or(NativeStateError::LimitExceeded)?;
        if max_unit_bytes == 0 || max_rows == 0 || max_total_bytes == 0 {
            return Err(NativeStateError::LimitExceeded);
        }

        let capture = self.terminal.capture(options)?;
        // SAFETY: `Capture` stores the native terminal object's stable C
        // pointer; its Rust lifetime is only the mutation exclusion proof. The
        // manager sets `capture_active` before returning and every actor path
        // buffers terminal mutations until finish/abort consumes this capture.
        // The manager and terminal outlive the actor-owned capture.
        let capture = unsafe {
            std::mem::transmute::<Capture<'_, 'static>, Capture<'static, 'static>>(capture)
        };
        self.capture_active = true;
        Ok(NativeManagedCapture {
            capture: Some(capture),
            max_record_bytes,
            detach_options: DetachOptions {
                max_pages,
                max_total_bytes,
                max_rows,
            },
            generation_bounds: NativeGenerationBounds {
                max_record_bytes: max_unit_bytes,
                max_rows,
                max_records,
                max_total_bytes,
            },
            ready_cursor: None,
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

    /// Install the sole continuation for a cursor-qualified generation.
    ///
    /// `reserved_bytes` is a hard retained-allocation budget covering both the
    /// fixed record table and every cached payload allocation. Installation
    /// owns the first generation reference.
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

        let records = NativeRecordTable::new(bounds.max_records)?;
        let payload_capacity = bounds.max_total_bytes;
        self.generations
            .try_reserve(1)
            .map_err(|_| NativeStateError::OutOfMemory)?;
        self.generations.insert(
            cursor,
            NativeCheckpointGeneration {
                continuation: Some(seed.continuation),
                records,
                bounds,
                cached_capacity: 0,
                payload_capacity,
                owners: 1,
                charge: Arc::new(NativeGenerationCharge::default()),
            },
        );
        Ok(())
    }

    /// Retain another actor-owned reference to an existing generation.
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

    /// Return cached index `N`, or append exactly one record at the frontier.
    ///
    /// Cache hits clone only the immutable `Bytes` handle. An index beyond the
    /// frontier is rejected. Allocation, row-bound, and native `OutOfSpace`
    /// failures happen before native advancement and leave the frontier fixed.
    pub(crate) fn history_record_at(
        &mut self,
        cursor: &OpaqueHistoryCursor,
        index: usize,
        requested_max_bytes: u32,
        requested_max_rows: u32,
    ) -> Result<CachedNativeHistoryRecord, NativeStateError> {
        let requested_bytes =
            usize::try_from(requested_max_bytes).map_err(|_| NativeStateError::LimitExceeded)?;
        let requested_rows =
            usize::try_from(requested_max_rows).map_err(|_| NativeStateError::LimitExceeded)?;
        if requested_bytes == 0 || requested_rows == 0 {
            return Err(NativeStateError::LimitExceeded);
        }

        let generation = self
            .generations
            .get_mut(cursor)
            .ok_or(NativeStateError::InvalidHandle)?;
        if let Some(record) = generation.records.get(index) {
            return record.for_request(requested_bytes, requested_rows);
        }
        if index != generation.records.len() || generation.continuation.is_none() {
            return Err(NativeStateError::InvalidHandle);
        }
        if generation.records.len() == generation.bounds.max_records {
            return Err(NativeStateError::LimitExceeded);
        }

        let remaining_capacity = generation
            .payload_capacity
            .checked_sub(generation.cached_capacity)
            .ok_or(NativeStateError::LimitExceeded)?;
        let max_bytes = generation.bounds.max_record_bytes;
        let max_rows = generation.bounds.max_rows;
        let output_len = max_bytes.min(remaining_capacity);
        if output_len == 0 {
            return Err(NativeStateError::LimitExceeded);
        }
        let mut scratch = Vec::new();
        scratch
            .try_reserve_exact(output_len)
            .map_err(|_| NativeStateError::OutOfMemory)?;
        scratch.resize(output_len, 0);

        let event = generation
            .continuation
            .as_mut()
            .ok_or(NativeStateError::InvalidHandle)?
            .next(ContinuationOptions { max_rows }, &mut scratch)?;
        if event.codec_version != CHECKPOINT_VERSION || event.record.len() > max_bytes {
            generation.continuation.take();
            return Err(NativeStateError::InvalidState);
        }
        let record_len = event.record.len();
        let (rows, finish) = match event.kind {
            CaptureEventKind::HistoryBegin { .. } => (0, false),
            CaptureEventKind::HistoryPage { .. } if event.rows <= max_rows => (event.rows, false),
            CaptureEventKind::Finish => (0, true),
            CaptureEventKind::Record
            | CaptureEventKind::Ready { .. }
            | CaptureEventKind::HistoryPage { .. } => {
                generation.continuation.take();
                return Err(NativeStateError::InvalidState);
            }
        };
        let cached_capacity = generation
            .cached_capacity
            .checked_add(record_len)
            .filter(|bytes| *bytes <= generation.payload_capacity)
            .ok_or(NativeStateError::LimitExceeded)?;
        scratch.truncate(record_len);
        let payload =
            ChargedNativePayload::new(scratch.into_boxed_slice(), Arc::clone(&generation.charge));
        let record = CachedNativeHistoryRecord {
            bytes: Bytes::from_owner(payload),
            rows,
            finish,
        };
        generation.records.push(record.clone());
        generation.cached_capacity = cached_capacity;
        if finish {
            generation.continuation.take();
        }
        record.for_request(requested_bytes, requested_rows)
    }

    /// Release one generation reference. Cached allocations that escaped through
    /// `Bytes` continue occupying this slot until their final clone is dropped.
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

#[derive(Debug)]
pub(crate) struct NativeManagedCapture<'manager> {
    capture: Option<Capture<'manager, 'static>>,
    max_record_bytes: usize,
    detach_options: DetachOptions,
    ready_cursor: Option<OpaqueHistoryCursor>,
    generation_bounds: NativeGenerationBounds,
}

impl NativeManagedCapture<'_> {
    pub(crate) const fn max_record_bytes(&self) -> usize {
        self.max_record_bytes
    }
    pub(crate) fn step<'buffer>(
        &mut self,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeCheckpointChunk<'buffer>, NativeStateError> {
        if self.ready_cursor.is_some() {
            return Err(NativeStateError::InvalidState);
        }
        let event = self
            .capture
            .as_mut()
            .ok_or(NativeStateError::InvalidState)?
            .next(buffer)?;
        if event.codec_version != CHECKPOINT_VERSION {
            return Err(NativeStateError::UnknownVersion);
        }
        if event.record.len() > self.max_record_bytes {
            return Err(NativeStateError::LimitExceeded);
        }
        let kind = match event.kind {
            CaptureEventKind::Record => NativeCheckpointChunkKind::Record,
            CaptureEventKind::Ready { checkpoint } => {
                self.ready_cursor = Some(*checkpoint.as_bytes());
                NativeCheckpointChunkKind::Ready
            }
            CaptureEventKind::HistoryBegin { .. }
            | CaptureEventKind::HistoryPage { .. }
            | CaptureEventKind::Finish => return Err(NativeStateError::InvalidState),
        };
        Ok(NativeCheckpointChunk {
            kind,
            codec_version: event.codec_version,
            bytes: event.record,
        })
    }

    pub(crate) fn detach_generation_ready(
        mut self,
    ) -> Result<(OpaqueHistoryCursor, NativeGenerationSeed), NativeStateError> {
        let cursor = self.ready_cursor.ok_or(NativeStateError::InvalidState)?;
        let capture = self.capture.take().ok_or(NativeStateError::InvalidState)?;
        let continuation = capture
            .detach_ready(self.detach_options)
            .map_err(|failure| failure.error)?;
        Ok((
            cursor,
            NativeGenerationSeed {
                continuation,
                bounds: self.generation_bounds,
            },
        ))
    }
}

fn require_protocol_07_native() -> Result<incremental::Capabilities, NativeStateError> {
    let engine = incremental::capabilities()?;
    if supports_protocol_07_native(&engine) {
        Ok(engine)
    } else {
        Err(NativeStateError::UnsupportedFeature)
    }
}

fn supports_protocol_07_native(engine: &incremental::Capabilities) -> bool {
    engine.version == INCREMENTAL_ABI_VERSION
        && engine.min_decode_version <= CHECKPOINT_VERSION
        && engine.max_decode_version >= CHECKPOINT_VERSION
        && engine.default_encode_version == CHECKPOINT_VERSION
        && engine.incremental
        && engine.ready
        && engine.history
        && engine.authenticated_tokens
        && engine.bounded_records
        && engine.bounded_pages
        && engine.bounded_units
        && engine.max_record_bytes > 0
        && engine.max_pages > 0
        && engine.max_unit_bytes > 0
        && engine.max_rows > 0
        && engine.codec_identity == CHECKPOINT_CODEC_IDENTITY
}

fn intersect_engine_limits(
    requested: BootstrapLimits,
    engine: &incremental::Capabilities,
) -> Option<BootstrapLimits> {
    let record_max = u32::try_from(engine.max_record_bytes).unwrap_or(u32::MAX);
    let unit_max = u32::try_from(engine.max_unit_bytes).unwrap_or(u32::MAX);
    BootstrapLimits::new(
        requested.max_chunk_bytes().min(record_max),
        requested.max_history_page_bytes().min(unit_max),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use allocator_api2::alloc::{AllocError, Allocator as RustAllocator, Layout};
    use libghostty_vt::TerminalOptions;
    use phux_protocol::caps::BootstrapProfileKind;
    use std::ptr::NonNull;

    #[derive(Clone, Copy, Debug)]
    struct AlwaysOom;

    // SAFETY: This allocator never returns an allocation, so no live memory
    // block or cross-clone ownership invariant can arise.
    unsafe impl RustAllocator for AlwaysOom {
        fn allocate(&self, _layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            Err(AllocError)
        }

        unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {
            unreachable!("AlwaysOom never allocates")
        }
    }

    fn terminal(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
        GhosttyTerminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 1000,
        })
        .expect("canonical terminal")
    }

    fn history_terminal() -> GhosttyTerminal<'static, 'static> {
        let mut terminal = terminal(20, 4);
        for row in 0..300 {
            terminal.vt_write(format!("history-{row:03}\r\n").as_bytes());
        }
        terminal
    }

    fn capture_to_ready(terminal: &mut GhosttyTerminal<'static, 'static>, limits: BootstrapLimits) {
        let mut capture =
            NativeCheckpointCapture::new(terminal, limits).expect("capture preflight");
        loop {
            let required = match capture.step(&mut []) {
                Err(NativeStateError::OutOfSpace {
                    required_bytes,
                    required_rows: 0,
                }) => required_bytes,
                other => panic!("zero-buffer bootstrap probe: {other:?}"),
            };
            assert!(required > 0);
            if required > 1 {
                let mut short = vec![0; required - 1];
                assert_eq!(
                    capture.step(&mut short).unwrap_err(),
                    NativeStateError::OutOfSpace {
                        required_bytes: required,
                        required_rows: 0,
                    }
                );
            }
            let mut exact = vec![0; required];
            let event = capture.step(&mut exact).expect("exact bootstrap record");
            assert_eq!(event.bytes.len(), required);
            if matches!(event.kind, NativeCheckpointChunkKind::Ready) {
                assert!(capture.is_ready());
                assert_eq!(
                    capture.step(&mut []).unwrap_err(),
                    NativeStateError::InvalidState,
                    "bootstrap host must never expose HISTORY or FINISH"
                );
                capture.abort().expect("release terminal after READY");
                return;
            }
        }
    }

    fn detach_managed_generation(
        manager: &mut NativeTerminalManager,
        limits: BootstrapLimits,
    ) -> (OpaqueHistoryCursor, NativeGenerationSeed) {
        let mut capture = manager
            .capture_generation_bounded(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
            .expect("generation capture preflight");
        loop {
            let required = match capture.step(&mut []) {
                Err(NativeStateError::OutOfSpace {
                    required_bytes,
                    required_rows: 0,
                }) => required_bytes,
                other => panic!("generation capture probe: {other:?}"),
            };
            let mut exact = vec![0; required];
            if matches!(
                capture.step(&mut exact).expect("generation record").kind,
                NativeCheckpointChunkKind::Ready
            ) {
                break;
            }
        }
        capture
            .detach_generation_ready()
            .expect("detach generation READY continuation")
    }

    #[test]
    fn advertises_exact_checkpoint_v2_with_indivisible_features() {
        let engine = incremental::capabilities().expect("incremental capability probe");
        assert!(supports_protocol_07_native(&engine));
        assert_eq!(engine.default_encode_version, CHECKPOINT_VERSION);
        assert_eq!(engine.codec_identity, CHECKPOINT_CODEC_IDENTITY);

        let advertised = native_bootstrap_capabilities();
        assert!(
            advertised
                .profiles
                .contains(BootstrapProfileKind::NativeState)
        );
        assert!(
            advertised
                .profiles
                .contains(BootstrapProfileKind::SynthesizedVtRaw)
        );
        assert!(
            advertised
                .profiles
                .contains(BootstrapProfileKind::SynthesizedVtStateSync)
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
    fn capture_page_bound_is_independent_of_prefix_record_size() {
        let engine = incremental::capabilities().expect("incremental capability probe");
        let narrow = bounded_capture_options(
            BootstrapLimits::new(
                phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES,
                phux_protocol::MAX_HISTORY_PAGE_BYTES,
            )
            .expect("default chunk bound"),
            &engine,
        )
        .expect("narrow capture options");
        let wide = bounded_capture_options(
            BootstrapLimits::new(
                phux_protocol::MAX_BOOTSTRAP_CHUNK_BYTES,
                phux_protocol::MAX_HISTORY_PAGE_BYTES,
            )
            .expect("maximum chunk bound"),
            &engine,
        )
        .expect("wide capture options");

        assert_eq!(wide.max_pages, narrow.max_pages);
        assert_eq!(wide.max_record_bytes, narrow.max_record_bytes);
        assert_eq!(
            wide.max_pages,
            CaptureOptions::default().max_pages.min(engine.max_pages)
        );
        let warm_history_pages = 50_000_usize.div_ceil(DetachOptions::default().max_rows);
        assert!(
            wide.max_pages >= warm_history_pages,
            "50k retained rows must not be rejected merely because prefix records permit \
             large payloads"
        );
        assert!(wide.max_record_bytes <= engine.max_record_bytes);
    }

    #[test]
    fn rejects_kitty_and_glyph_state_before_first_record() {
        let mut kitty = terminal(10, 5);
        phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut kitty)
            .expect("kitty configuration");
        kitty.resize(10, 5, 8, 16).expect("cell geometry");
        kitty.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,i=9,q=2;AAECAw==\x1b\\");
        assert!(matches!(
            NativeCheckpointCapture::new(&mut kitty, BootstrapLimits::default()),
            Err(NativeStateError::UnsupportedFeature)
        ));

        let mut glyph = terminal(10, 5);
        glyph.vt_write(b"\x1b_25a1;r;cp=e0a0;AAAAAAAAAAAAAA==\x1b\\");
        assert!(matches!(
            NativeCheckpointCapture::new(&mut glyph, BootstrapLimits::default()),
            Err(NativeStateError::UnsupportedFeature)
        ));
    }

    #[test]
    fn ready_handoff_pages_bounded_history_while_live_output_continues() {
        let limits = BootstrapLimits::new(phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES, 64 * 1024)
            .expect("test limits");
        let mut source = history_terminal();
        capture_to_ready(&mut source, limits);
        let mut history = NativeHistoryCursor::new(source, limits).expect("owned history cursor");
        let capability = *history.cursor();
        history.vt_write(b"live raw PTY bytes after READY\r\n");

        let mut pages = 0usize;
        loop {
            let required = match history.next(limits.max_history_page_bytes(), &mut []) {
                Ok(NativeHistoryEvent::End) => break,
                Err(NativeStateError::OutOfSpace {
                    required_bytes,
                    required_rows: 0,
                }) => required_bytes,
                other => panic!("zero-buffer history probe: {other:?}"),
            };
            assert!(required > 0);
            if required > 1 {
                let mut short = vec![0; required - 1];
                assert_eq!(
                    history
                        .next(limits.max_history_page_bytes(), &mut short)
                        .unwrap_err(),
                    NativeStateError::OutOfSpace {
                        required_bytes: required,
                        required_rows: 0,
                    }
                );
            }
            let mut exact = vec![0; required];
            let event = history
                .next(limits.max_history_page_bytes(), &mut exact)
                .expect("bounded history page");
            let NativeHistoryEvent::Page {
                bytes, next_cursor, ..
            } = event
            else {
                panic!("probe promised a history page");
            };
            assert_eq!(bytes.len(), required);
            assert!(
                bytes.len()
                    <= usize::try_from(limits.max_history_page_bytes()).expect("native usize")
            );
            assert_eq!(next_cursor, capability);
            pages += 1;
        }
        assert!(pages > 0);

        let mut source = history.into_terminal();
        source.vt_write(b"still raw after history end");
        NativeCheckpointCapture::new(&mut source, limits)
            .expect("history cursor released canonical terminal");
    }

    #[test]
    fn abort_failed_construction_and_oom_status_preserve_cleanup_contract() {
        let mut source = terminal(20, 4);
        source.vt_write(b"checkpoint-state");
        let mut capture =
            NativeCheckpointCapture::new(&mut source, BootstrapLimits::default()).expect("capture");
        let _ = capture.step(&mut []);
        capture.abort().expect("explicit abort");
        source.vt_write(b"-after-abort");

        let tiny = BootstrapLimits::new(1, 1).expect("protocol-valid tiny bounds");
        NativeCheckpointCapture::new(&mut source, tiny)
            .expect("transport chunk size does not constrain native record size")
            .abort()
            .expect("tiny transport capture abort");
        source.vt_write(b"-still-live");
        NativeCheckpointCapture::new(&mut source, BootstrapLimits::default())
            .expect("failed construction released resources");
        let allocator = libghostty_vt::alloc::Allocator::from(AlwaysOom);
        assert_eq!(
            source
                .capture_with_alloc(&allocator, CaptureOptions::default())
                .unwrap_err(),
            NativeStateError::OutOfMemory
        );
        source.vt_write(b"-after-oom");
        NativeCheckpointCapture::new(&mut source, BootstrapLimits::default())
            .expect("OOM construction released the canonical terminal");
    }

    #[test]
    fn history_pruned_reset_and_resize_remain_distinct_native_errors() {
        let limits = BootstrapLimits::default();
        let size = usize::try_from(limits.max_history_page_bytes()).expect("native usize");
        let mut buffer = vec![0; size];
        assert_eq!(
            NativeStateError::Stale,
            incremental::Error::Stale,
            "the exact stale status remains public even though the safe owned \
             cursor keeps its lease live"
        );

        let mut stale = NativeHistoryCursor::new(history_terminal(), limits).expect("stale cursor");
        stale.vt_write(b"\x1b[3J");
        assert_eq!(
            stale
                .next(limits.max_history_page_bytes(), &mut buffer)
                .unwrap_err(),
            NativeStateError::Stale
        );

        let mut pruned =
            NativeHistoryCursor::new(history_terminal(), limits).expect("pruned cursor");
        for _ in 0..2_000 {
            pruned.vt_write(b"history pressure\r\n");
        }
        assert_eq!(
            pruned
                .next(limits.max_history_page_bytes(), &mut buffer)
                .unwrap_err(),
            NativeStateError::Pruned
        );

        let mut reset = NativeHistoryCursor::new(history_terminal(), limits).expect("reset cursor");
        reset.reset();
        assert_eq!(
            reset
                .next(limits.max_history_page_bytes(), &mut buffer)
                .unwrap_err(),
            NativeStateError::Reset
        );

        let mut resized =
            NativeHistoryCursor::new(history_terminal(), limits).expect("resize cursor");
        resized.resize(21, 4, 8, 16).expect("resize live source");
        assert_eq!(
            resized
                .next(limits.max_history_page_bytes(), &mut buffer)
                .unwrap_err(),
            NativeStateError::Resize
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "walks one generation through its whole life -- probe, cache, advance to FINISH, then release both owners -- and the assertions only mean anything in that order"
    )]
    fn shared_generation_caches_frontier_once_and_drops_after_last_owner() {
        let limits = BootstrapLimits::new(phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES, 64 * 1024)
            .expect("test limits");
        let mut manager =
            NativeTerminalManager::new(history_terminal(), 1).expect("native manager");
        let (cursor, seed) = detach_managed_generation(&mut manager, limits);
        let bounds = seed.bounds();
        let reserved_bytes = bounds
            .required_reserved_bytes()
            .expect("bounded generation reservation");
        manager
            .install_generation(cursor, seed, bounds, reserved_bytes)
            .expect("install shared generation");
        manager
            .retain_generation(&cursor)
            .expect("second generation owner");

        let (required_bytes, required_rows) = match manager.history_record_at(&cursor, 0, 1, 1) {
            Err(NativeStateError::OutOfSpace {
                required_bytes,
                required_rows,
            }) => (required_bytes, required_rows),
            other => panic!("short exact frontier probe: {other:?}"),
        };
        assert!(required_bytes > 1);
        assert_eq!(
            required_rows, 0,
            "the zero-row HISTORY_BEGIN requirement must come from the native frontier"
        );
        assert_eq!(
            manager
                .generations
                .get(&cursor)
                .expect("installed generation")
                .records
                .len(),
            1,
            "a short-buffer probe still caches the record it measured: reporting \
             `required_bytes` means the record was already pulled off the \
             continuation, and dropping it there would lose it outright. The \
             retry below is served from this cache — which is what makes the \
             pointer-equality assertion further down meaningful."
        );

        let first = manager
            .history_record_at(
                &cursor,
                0,
                u32::try_from(required_bytes).expect("protocol byte bound"),
                1,
            )
            .expect("row-bounded HISTORY_BEGIN");
        assert_eq!(first.rows, 0);
        let repeated = manager
            .history_record_at(&cursor, 0, limits.max_history_page_bytes(), 1)
            .expect("cached first record");
        assert_eq!(first, repeated);
        assert_eq!(
            first.bytes.as_ptr(),
            repeated.bytes.as_ptr(),
            "cache hits must share the immutable record allocation"
        );
        assert_eq!(
            manager
                .generations
                .get(&cursor)
                .expect("installed generation")
                .records
                .len(),
            1,
            "rereading an index must not append a second record"
        );

        let max_rows = u32::try_from(bounds.max_rows).expect("protocol row bound");
        let mut index = 1;
        loop {
            let record = manager
                .history_record_at(&cursor, index, limits.max_history_page_bytes(), max_rows)
                .expect("advance shared frontier");
            index = index.checked_add(1).expect("bounded record index");
            if record.finish {
                break;
            }
        }
        assert!(
            manager
                .generations
                .get(&cursor)
                .expect("finished cache remains owned")
                .continuation
                .is_none(),
            "FINISH must release the native continuation immediately"
        );

        let escaped = first.bytes.clone();
        drop(first);
        drop(repeated);
        manager
            .release_generation(&cursor)
            .expect("release first owner");
        assert!(manager.generations.contains_key(&cursor));
        manager
            .release_generation(&cursor)
            .expect("release last owner");
        assert!(!manager.generations.contains_key(&cursor));
        assert_eq!(manager.retired_generation_charges.len(), 1);
        assert!(
            manager.retired_generation_charges[0]
                .live_payload_bytes
                .load(Ordering::Acquire)
                >= escaped.len()
        );
        let (blocked_cursor, blocked_seed) = detach_managed_generation(&mut manager, limits);
        let blocked_bounds = blocked_seed.bounds();
        assert_eq!(
            manager
                .install_generation(
                    blocked_cursor,
                    blocked_seed,
                    blocked_bounds,
                    blocked_bounds
                        .required_reserved_bytes()
                        .expect("blocked generation reservation"),
                )
                .unwrap_err(),
            NativeStateError::LimitExceeded
        );
        assert!(matches!(
            manager.capture(limits),
            Err(NativeStateError::LimitExceeded)
        ));
        drop(escaped);
        manager
            .capture(limits)
            .expect("final escaped Bytes drop restores bounded capacity");
    }

    #[test]
    fn generation_bounds_ignore_connection_page_limits() {
        let small = BootstrapLimits::new(phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES, 64 * 1024)
            .expect("small connection limits");
        let large = BootstrapLimits::new(
            phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES,
            phux_protocol::MAX_HISTORY_PAGE_BYTES,
        )
        .expect("large connection limits");
        let mut manager =
            NativeTerminalManager::new(history_terminal(), 1).expect("native manager");

        let (small_cursor, small_seed) = detach_managed_generation(&mut manager, small);
        let (large_cursor, large_seed) = detach_managed_generation(&mut manager, large);
        assert_eq!(small_cursor, large_cursor);
        assert_eq!(small_seed.bounds(), large_seed.bounds());
        assert_eq!(
            small_seed.bounds().max_record_bytes,
            manager.engine.max_unit_bytes.min(
                usize::try_from(phux_protocol::MAX_HISTORY_PAGE_BYTES)
                    .expect("protocol byte bound")
            )
        );
    }

    #[test]
    fn generation_install_requires_exact_seed_bounds_and_full_reservation() {
        let limits = BootstrapLimits::default();
        let mut manager =
            NativeTerminalManager::new(history_terminal(), 1).expect("native manager");

        let (cursor, seed) = detach_managed_generation(&mut manager, limits);
        let bounds = seed.bounds();
        let incompatible = NativeGenerationBounds {
            max_records: bounds
                .max_records
                .checked_sub(1)
                .expect("seed has HISTORY_BEGIN and FINISH slots"),
            ..bounds
        };
        assert_eq!(
            manager
                .install_generation(
                    cursor,
                    seed,
                    incompatible,
                    bounds
                        .required_reserved_bytes()
                        .expect("generation reservation"),
                )
                .unwrap_err(),
            NativeStateError::LimitExceeded
        );

        let (cursor, seed) = detach_managed_generation(&mut manager, limits);
        let required = seed
            .bounds()
            .required_reserved_bytes()
            .expect("generation reservation");
        assert_eq!(
            manager
                .install_generation(
                    cursor,
                    seed,
                    bounds,
                    required.checked_sub(1).expect("nonzero reservation"),
                )
                .unwrap_err(),
            NativeStateError::LimitExceeded
        );
        assert!(manager.generations.is_empty());
    }

    #[test]
    fn full_generation_cache_allows_existing_cursor_join_only() {
        let limits = BootstrapLimits::default();
        let mut manager =
            NativeTerminalManager::new(history_terminal(), 1).expect("native manager");
        let (cursor, seed) = detach_managed_generation(&mut manager, limits);
        let bounds = seed.bounds();
        manager
            .install_generation(
                cursor,
                seed,
                bounds,
                bounds
                    .required_reserved_bytes()
                    .expect("generation reservation"),
            )
            .expect("install sole generation");

        let (joining_cursor, joining_seed) = detach_managed_generation(&mut manager, limits);
        assert_eq!(joining_cursor, cursor);
        drop(joining_seed);
        manager
            .retain_generation(&joining_cursor)
            .expect("existing cursor joins at full capacity");

        manager.vt_write(b"new active state");
        let (new_cursor, new_seed) = detach_managed_generation(&mut manager, limits);
        assert_ne!(new_cursor, cursor);
        let new_bounds = new_seed.bounds();
        assert_eq!(
            manager
                .install_generation(
                    new_cursor,
                    new_seed,
                    new_bounds,
                    new_bounds
                        .required_reserved_bytes()
                        .expect("new generation reservation"),
                )
                .unwrap_err(),
            NativeStateError::LimitExceeded
        );
        manager
            .release_generation(&cursor)
            .expect("release original owner");
        manager
            .release_generation(&cursor)
            .expect("release joining owner");
    }
}
