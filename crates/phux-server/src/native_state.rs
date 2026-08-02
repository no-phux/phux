//! Native libghostty checkpoint capability probing and bounded state hosts.
//!
//! Native bootstrap publishes only the checkpoint prefix through the codec's
//! authenticated READY record. Retained history is a separate, client-pulled
//! stream backed by owned native continuations, so ordinary PTY output can
//! continue while older history is paged. Records and tokens remain opaque.

use std::collections::HashMap;

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
        let max_record_bytes = usize::try_from(limits.max_chunk_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?
            .min(engine.max_record_bytes);
        let max_pages = CaptureOptions::default().max_pages.min(engine.max_pages);
        if max_record_bytes == 0 || max_pages == 0 {
            return Err(NativeStateError::LimitExceeded);
        }

        let capture = terminal.capture(CaptureOptions {
            max_record_bytes,
            max_pages,
        })?;
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
pub struct NativeHistoryCursor<'terminal_alloc: 'cb, 'cb> {
    inner: LiveHistoryCursor<'terminal_alloc, 'cb, 'static>,
    max_unit_bytes: usize,
    max_rows: usize,
    max_units: usize,
}

impl<'terminal_alloc: 'cb, 'cb> NativeHistoryCursor<'terminal_alloc, 'cb> {
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

/// One terminal-independent continuation record returned to the actor.
#[derive(Debug)]
pub(crate) enum NativeManagedHistoryEvent<'buffer> {
    Page {
        bytes: &'buffer [u8],
        rows: usize,
        next_cursor: OpaqueHistoryCursor,
    },
    Finish {
        bytes: &'buffer [u8],
    },
}

/// Actor-owned terminal and bounded concurrent native history cuts.
#[derive(Debug)]
pub(crate) struct NativeTerminalManager {
    terminal: GhosttyTerminal<'static, 'static>,
    continuations: HashMap<(u64, OpaqueHistoryCursor), CaptureContinuation<'static>>,
    capacity: usize,
    engine: incremental::Capabilities,
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
        let mut continuations = HashMap::new();
        if continuations.try_reserve(capacity).is_err() {
            return Err(NativeManagerInitFailure {
                error: NativeStateError::OutOfMemory,
                terminal,
            });
        }
        Ok(Self {
            terminal,
            continuations,
            capacity,
            engine,
        })
    }

    pub(crate) fn terminal(&self) -> &GhosttyTerminal<'static, 'static> {
        &self.terminal
    }

    pub(crate) fn vt_write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    pub(crate) fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> libghostty_vt::error::Result<()> {
        self.continuations.clear();
        self.terminal
            .resize(cols, rows, cell_width_px, cell_height_px)
    }

    pub(crate) fn reset(&mut self) {
        self.continuations.clear();
        self.terminal.reset();
    }

    pub(crate) fn capture(
        &mut self,
        limits: BootstrapLimits,
    ) -> Result<NativeManagedCapture<'_>, NativeStateError> {
        if self.continuations.len() == self.capacity {
            return Err(NativeStateError::LimitExceeded);
        }
        let max_record_bytes = usize::try_from(limits.max_chunk_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?
            .min(self.engine.max_record_bytes);
        let max_unit_bytes = usize::try_from(limits.max_history_page_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?
            .min(self.engine.max_unit_bytes);
        let capture_defaults = CaptureOptions::default();
        let detach_defaults = DetachOptions::default();
        let max_pages = capture_defaults.max_pages.min(self.engine.max_pages);
        let protocol_max_rows = usize::try_from(phux_protocol::MAX_HISTORY_PAGE_ROWS)
            .map_err(|_| NativeStateError::LimitExceeded)?;
        let max_rows = detach_defaults
            .max_rows
            .min(self.engine.max_rows)
            .min(protocol_max_rows);
        let negotiated_total_bytes = max_unit_bytes
            .checked_mul(max_pages)
            .ok_or(NativeStateError::LimitExceeded)?;
        let max_total_bytes = detach_defaults.max_total_bytes.min(negotiated_total_bytes);
        if max_record_bytes == 0
            || max_unit_bytes == 0
            || max_pages == 0
            || max_rows == 0
            || max_total_bytes == 0
        {
            return Err(NativeStateError::LimitExceeded);
        }
        let capture = self.terminal.capture(CaptureOptions {
            max_record_bytes,
            max_pages,
        })?;
        Ok(NativeManagedCapture {
            capture: Some(capture),
            max_record_bytes,
            detach_options: DetachOptions {
                max_pages,
                max_total_bytes,
                max_rows,
            },
            ready_cursor: None,
        })
    }

    pub(crate) fn retain(
        &mut self,
        owner: u64,
        cursor: OpaqueHistoryCursor,
        continuation: CaptureContinuation<'static>,
    ) -> Result<(), NativeStateError> {
        if self.continuations.len() == self.capacity
            || self.continuations.contains_key(&(owner, cursor))
        {
            return Err(NativeStateError::LimitExceeded);
        }
        self.continuations.insert((owner, cursor), continuation);
        Ok(())
    }

    pub(crate) fn next<'buffer>(
        &mut self,
        owner: u64,
        cursor: &OpaqueHistoryCursor,
        limits: BootstrapLimits,
        requested_max_bytes: u32,
        requested_max_rows: u32,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeManagedHistoryEvent<'buffer>, NativeStateError> {
        let negotiated = usize::try_from(limits.max_history_page_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?
            .min(self.engine.max_unit_bytes);
        let requested =
            usize::try_from(requested_max_bytes).map_err(|_| NativeStateError::LimitExceeded)?;
        let requested_rows =
            usize::try_from(requested_max_rows).map_err(|_| NativeStateError::LimitExceeded)?;
        let max_bytes = negotiated.min(requested);
        let protocol_max_rows = usize::try_from(phux_protocol::MAX_HISTORY_PAGE_ROWS)
            .map_err(|_| NativeStateError::LimitExceeded)?;
        let max_rows = requested_rows
            .min(self.engine.max_rows)
            .min(protocol_max_rows);
        if max_bytes == 0 || max_rows == 0 {
            return Err(NativeStateError::LimitExceeded);
        }
        let continuation = self
            .continuations
            .get_mut(&(owner, *cursor))
            .ok_or(NativeStateError::InvalidHandle)?;
        let output_len = buffer.len().min(max_bytes);
        let event =
            continuation.next(ContinuationOptions { max_rows }, &mut buffer[..output_len])?;
        if event.codec_version != CHECKPOINT_VERSION || event.record.len() > max_bytes {
            return Err(NativeStateError::InvalidState);
        }
        match event.kind {
            CaptureEventKind::HistoryBegin { .. } => Ok(NativeManagedHistoryEvent::Page {
                bytes: event.record,
                rows: 0,
                next_cursor: *cursor,
            }),
            CaptureEventKind::HistoryPage { .. } => Ok(NativeManagedHistoryEvent::Page {
                bytes: event.record,
                rows: event.rows,
                next_cursor: *cursor,
            }),
            CaptureEventKind::Finish => Ok(NativeManagedHistoryEvent::Finish {
                bytes: event.record,
            }),
            CaptureEventKind::Record | CaptureEventKind::Ready { .. } => {
                Err(NativeStateError::InvalidState)
            }
        }
    }

    pub(crate) fn release(
        &mut self,
        owner: u64,
        cursor: &OpaqueHistoryCursor,
    ) -> Result<(), NativeStateError> {
        self.continuations
            .remove(&(owner, *cursor))
            .map(drop)
            .ok_or(NativeStateError::InvalidHandle)
    }

    pub(crate) fn into_terminal(self) -> GhosttyTerminal<'static, 'static> {
        let Self {
            terminal,
            continuations,
            ..
        } = self;
        drop(continuations);
        terminal
    }
}

#[derive(Debug)]
pub(crate) struct NativeManagedCapture<'manager> {
    capture: Option<Capture<'manager, 'static>>,
    max_record_bytes: usize,
    detach_options: DetachOptions,
    ready_cursor: Option<OpaqueHistoryCursor>,
}

impl NativeManagedCapture<'_> {
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

    pub(crate) fn detach_ready(
        mut self,
    ) -> Result<(OpaqueHistoryCursor, CaptureContinuation<'static>), NativeStateError> {
        let cursor = self.ready_cursor.ok_or(NativeStateError::InvalidState)?;
        let capture = self.capture.take().ok_or(NativeStateError::InvalidState)?;
        let continuation = capture
            .detach_ready(self.detach_options)
            .map_err(|failure| failure.error)?;
        Ok((cursor, continuation))
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

    fn detach_managed(
        manager: &mut NativeTerminalManager,
        limits: BootstrapLimits,
    ) -> (OpaqueHistoryCursor, CaptureContinuation<'static>) {
        let mut capture = manager.capture(limits).expect("managed capture preflight");
        loop {
            let required = match capture.step(&mut []) {
                Err(NativeStateError::OutOfSpace {
                    required_bytes,
                    required_rows: 0,
                }) => required_bytes,
                other => panic!("managed capture probe: {other:?}"),
            };
            let mut exact = vec![0; required];
            let event = capture.step(&mut exact).expect("managed record");
            if matches!(event.kind, NativeCheckpointChunkKind::Ready) {
                break;
            }
        }
        capture.detach_ready().expect("detach READY continuation")
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
        let limits = BootstrapLimits::new(64 * 1024, 64 * 1024).expect("test limits");
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
        assert!(matches!(
            NativeCheckpointCapture::new(&mut source, tiny),
            Err(NativeStateError::LimitExceeded)
        ));
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

        let mut pruned =
            NativeHistoryCursor::new(history_terminal(), limits).expect("pruned cursor");
        pruned.vt_write(b"\x1b[3J");
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
    fn manager_bounds_retained_cuts_and_keeps_live_terminal_writable() {
        let limits = BootstrapLimits::new(64 * 1024, 64 * 1024).expect("test limits");
        let mut manager =
            NativeTerminalManager::new(history_terminal(), 1).expect("native manager");
        let (cursor, continuation) = detach_managed(&mut manager, limits);
        manager
            .retain(1, cursor, continuation)
            .expect("retain detached continuation");
        manager.vt_write(b"live PTY bytes while retained history is leased\r\n");
        assert!(matches!(
            manager.capture(limits),
            Err(NativeStateError::LimitExceeded)
        ));

        let mut page = vec![
            0;
            usize::try_from(limits.max_history_page_bytes())
                .expect("native history bound")
        ];
        assert!(matches!(
            manager
                .next(
                    1,
                    &cursor,
                    limits,
                    limits.max_history_page_bytes(),
                    u32::MAX,
                    &mut page,
                )
                .expect("retained history page"),
            NativeManagedHistoryEvent::Page { .. } | NativeManagedHistoryEvent::Finish { .. }
        ));
        manager.release(1, &cursor).expect("release retained cut");
        manager
            .capture(limits)
            .expect("released capacity admits next capture");
    }

    #[test]
    fn equal_checkpoint_tokens_are_isolated_by_server_owner() {
        let limits = BootstrapLimits::new(64 * 1024, 64 * 1024).expect("test limits");
        let mut manager =
            NativeTerminalManager::new(history_terminal(), 2).expect("native manager");
        let (first_cursor, first) = detach_managed(&mut manager, limits);
        manager.retain(1, first_cursor, first).expect("first owner");
        let (second_cursor, second) = detach_managed(&mut manager, limits);
        assert_eq!(
            first_cursor, second_cursor,
            "checkpoint authentication is content-derived, not an owner id"
        );
        manager
            .retain(2, second_cursor, second)
            .expect("second owner with same opaque checkpoint");

        let size = usize::try_from(limits.max_history_page_bytes()).expect("history bound");
        let mut first_bytes = vec![0; size];
        let mut second_bytes = vec![0; size];
        let first = manager
            .next(
                1,
                &first_cursor,
                limits,
                limits.max_history_page_bytes(),
                u32::MAX,
                &mut first_bytes,
            )
            .expect("first owner continuation");
        let second = manager
            .next(
                2,
                &second_cursor,
                limits,
                limits.max_history_page_bytes(),
                u32::MAX,
                &mut second_bytes,
            )
            .expect("second owner continuation");
        let (
            NativeManagedHistoryEvent::Page {
                bytes: first,
                rows: first_rows,
                ..
            },
            NativeManagedHistoryEvent::Page {
                bytes: second,
                rows: second_rows,
                ..
            },
        ) = (first, second)
        else {
            panic!("both owners begin independent continuations");
        };
        assert_eq!(first, second);
        assert_eq!(first_rows, second_rows);
        manager.release(1, &first_cursor).expect("first release");
        manager.release(2, &second_cursor).expect("second release");
    }
}
