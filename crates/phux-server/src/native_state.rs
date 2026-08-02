//! Native libghostty checkpoint capability probing and bounded state hosts.
//!
//! Native bootstrap publishes only the checkpoint prefix through the codec's
//! authenticated READY record. Retained history is a separate, client-pulled
//! stream backed by an owned live cursor, so ordinary PTY output can continue
//! while older history is paged. All codec records and tokens remain opaque.

use libghostty_vt::{
    Terminal as GhosttyTerminal,
    snapshot::incremental::{
        self, Capture, CaptureEventKind, CaptureOptions, HistoryEvent, HistoryOptions,
        LiveHistoryCursor, ScreenKey, TOKEN_LEN,
    },
};
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, EngineCodec, EngineFeatureSet,
};

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
    Ready {
        /// Opaque digest authenticating the exact terminal cut through READY.
        checkpoint: OpaqueHistoryCursor,
    },
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
            CaptureEventKind::Ready { checkpoint } => {
                self.ready = true;
                NativeCheckpointChunkKind::Ready {
                    checkpoint: *checkpoint.as_bytes(),
                }
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
    use libghostty_vt::TerminalOptions;
    use phux_protocol::caps::BootstrapProfileKind;

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

    fn capture_to_ready(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        limits: BootstrapLimits,
    ) -> OpaqueHistoryCursor {
        let mut capture = NativeCheckpointCapture::new(terminal, limits).expect("capture preflight");
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
            if let NativeCheckpointChunkKind::Ready { checkpoint } = event.kind {
                assert!(capture.is_ready());
                assert_eq!(
                    capture.step(&mut []).unwrap_err(),
                    NativeStateError::InvalidState,
                    "bootstrap host must never expose HISTORY or FINISH"
                );
                capture.abort().expect("release terminal after READY");
                return checkpoint;
            }
        }
    }

    #[test]
    fn advertises_exact_checkpoint_v2_with_indivisible_features() {
        let engine = incremental::capabilities().expect("incremental capability probe");
        assert!(supports_protocol_07_native(&engine));
        assert_eq!(engine.default_encode_version, CHECKPOINT_VERSION);
        assert_eq!(engine.codec_identity, CHECKPOINT_CODEC_IDENTITY);

        let advertised = native_bootstrap_capabilities();
        assert!(advertised.profiles.contains(BootstrapProfileKind::NativeState));
        assert!(advertised.profiles.contains(BootstrapProfileKind::SynthesizedVtRaw));
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
        assert_eq!(advertised.native_features, EngineFeatureSet::required_native());
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
        let ready_checkpoint = capture_to_ready(&mut source, limits);
        let mut history = NativeHistoryCursor::new(source, limits).expect("owned history cursor");
        assert_eq!(history.checkpoint(), &ready_checkpoint, "same terminal cut");
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
                bytes,
                next_cursor,
                ..
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
        let mut capture = NativeCheckpointCapture::new(
            &mut source,
            BootstrapLimits::default(),
        )
        .expect("capture");
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
        assert_eq!(NativeStateError::OutOfMemory, incremental::Error::OutOfMemory);
    }

    #[test]
    fn history_stale_reset_and_resize_remain_distinct_native_errors() {
        let limits = BootstrapLimits::default();
        let size = usize::try_from(limits.max_history_page_bytes()).expect("native usize");
        let mut buffer = vec![0; size];

        let mut stale = NativeHistoryCursor::new(history_terminal(), limits).expect("stale cursor");
        stale.vt_write(b"\x1b[3J");
        assert_eq!(
            stale.next(limits.max_history_page_bytes(), &mut buffer)
                .unwrap_err(),
            NativeStateError::Stale
        );

        let mut reset = NativeHistoryCursor::new(history_terminal(), limits).expect("reset cursor");
        reset.reset();
        assert_eq!(
            reset.next(limits.max_history_page_bytes(), &mut buffer)
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
}
