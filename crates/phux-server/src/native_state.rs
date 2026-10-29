//! Native checkpoint hosts over libghostty's GHOSTSNP snapshot codec.
//!
//! Prefix capture advances one bounded record at a time through READY;
//! detaching READY registers a history cut in O(1), and each later
//! `HISTORY_REQUEST` borrows the live terminal for one bounded step, so PTY
//! output continues between pulls. Records are forwarded without decoding.

use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use libghostty_vt::{
    Error as GhosttyError, Terminal as GhosttyTerminal,
    snapshot::{CaptureEvent, CaptureInvalidation, CaptureOptions, HistoryCapture, OwnedCapture},
};
use phux_protocol::caps::{BootstrapCapabilities, BootstrapLimits, EngineCodec, EngineFeatureSet};
use thiserror::Error;

/// Opaque manager-local generation identity: SHA-256 of READY bytes and nonce.
pub const TOKEN_LEN: usize = 32;
const PROGRESSIVE_CHECKPOINT_VERSION: u16 = EngineCodec::LibghosttySnapshotV1 as u16;
/// Continuation tracking limit so a frozen capture can include mid-sequence VT.
const CONTINUATION_LIMIT: usize = 64 * 1024 * 1024;

/// Greatest number of opaque codec records retained before READY publication.
pub(crate) const MAX_NATIVE_PREFIX_CHUNKS: usize = 4_096;
/// Greatest aggregate opaque codec payload retained before READY publication.
pub(crate) const MAX_NATIVE_PREFIX_BYTES: usize = 64 * 1024 * 1024;
/// Frames a native bootstrap spends beyond its chunks: one BEGIN and one READY.
pub(crate) const NATIVE_BOOTSTRAP_FRAMING_FRAMES: usize = 2;
/// Frame budget of a full native bootstrap: every prefix chunk plus framing.
pub(crate) const MAX_NATIVE_BOOTSTRAP_FRAMES: usize =
    MAX_NATIVE_PREFIX_CHUNKS + NATIVE_BOOTSTRAP_FRAMING_FRAMES;

/// Failures from native capture, history, and generation management.
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
            // `InvalidValue`, and result codes libghostty adds later
            // (`Error` is non-exhaustive), such as `Rejected`.
            _ => Self::UnsupportedFeature,
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
    progressive_ready_available(&mut terminal)
}

fn progressive_ready_available(terminal: &mut GhosttyTerminal<'static, 'static>) -> bool {
    const PROBE_RECORD_BYTES: usize = 4096;
    let Ok(mut capture) = terminal.capture_snapshot(CaptureOptions {
        max_record_bytes: PROBE_RECORD_BYTES,
        max_pages: 8,
    }) else {
        return false;
    };
    let mut buffer = vec![0; PROBE_RECORD_BYTES];
    loop {
        match capture.next(&mut buffer) {
            Ok(CaptureEvent::Ready { written }) => return written > 0,
            Ok(CaptureEvent::Record { .. }) => {}
            _ => return false,
        }
    }
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

/// The continuation of a capture that reached READY: a leased engine cut
/// (no pages copied), installed back into the actor-local manager.
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
    /// Why the cut was spent before FINISH, reported to every later request.
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

/// Actor-owned terminal and bounded concurrent history cuts. `Drop` releases
/// the cuts first, since their pins live in the terminal's page list.
#[derive(Debug)]
pub(crate) struct NativeTerminalManager {
    terminal: Option<GhosttyTerminal<'static, 'static>>,
    generations: HashMap<OpaqueHistoryCursor, NativeCheckpointGeneration>,
    retired_generation_charges: Vec<Arc<NativeGenerationCharge>>,
    capacity: usize,
    capture_active: bool,
    next_generation: u64,
}

impl Drop for NativeTerminalManager {
    fn drop(&mut self) {
        // Release cuts before the terminal (they untrack its pins).
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

mod capture;
mod manager;

#[cfg(test)]
mod tests;
