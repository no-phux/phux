//! Native libghostty checkpoint capability probing and bounded capture.
//!
//! This module is compiled only for native builds with the `native-engine`
//! feature. Opaque checkpoint bytes and authenticated tokens remain owned by
//! libghostty's safe API; this host only applies protocol-negotiated bounds and
//! translates event metadata into protocol-facing names.

use libghostty_vt::{
    Terminal as GhosttyTerminal,
    snapshot::incremental::{
        self, Capture, CaptureEventKind, CaptureOptions, CheckpointToken, ScreenKey,
    },
};
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, EngineCodec, EngineFeatureSet,
};

/// Exact incremental ABI version hosted by this protocol integration.
const INCREMENTAL_ABI_VERSION: u32 = 1;
/// Exact immutable checkpoint envelope version selected by protocol 0.7.
const CHECKPOINT_VERSION: u16 = EngineCodec::LibghosttyCheckpointV2 as u16;
/// Stable identity reported by the owned libghostty checkpoint implementation.
const CHECKPOINT_CODEC_IDENTITY: &str = "ghostty.snapshot.v1-v2.incremental.v1";

/// Exact status reported by libghostty's incremental checkpoint API.
///
/// This is a public rename, not a translated error. Callers can match
/// `UnsupportedFeature`, `OutOfSpace`, `LimitExceeded`, `OutOfMemory`, `Stale`,
/// `Reset`, `Resize`, and every other native status without parsing a string.
pub use incremental::Error as NativeStateError;

/// Probe the linked engine and advertise native checkpoint v2 only when its
/// complete protocol-0.7 contract is present.
///
/// A failed or incomplete probe leaves both synthesized VT profiles available
/// and advertises no native codec or native feature subset. Payload limits are
/// intersected with the engine's strict record and history-unit maxima.
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

/// Typed metadata for one complete opaque native checkpoint record.
#[derive(Debug)]
pub enum NativeCheckpointChunkKind {
    /// Envelope or non-boundary state record.
    Record,
    /// Authenticated renderable boundary.
    Ready {
        /// Opaque authenticated checkpoint capability for the READY prefix.
        checkpoint: CheckpointToken,
    },
    /// Start of one screen's history page sequence.
    HistoryBegin {
        /// Engine-owned screen generation key.
        screen: ScreenKey,
        /// Total history pages declared for this screen.
        page_count: u32,
    },
    /// One complete independently framed history page.
    HistoryPage {
        /// Engine-owned screen generation key.
        screen: ScreenKey,
        /// Zero-based page index.
        index: u32,
        /// Total history pages declared for this screen.
        page_count: u32,
    },
    /// Authenticated end of exactly one checkpoint.
    Finish,
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

/// RAII host for one bounded incremental checkpoint capture.
///
/// Construction mutably borrows the canonical terminal and asks libghostty to
/// validate all terminal-owned semantic state before returning. A caller may
/// therefore publish `BOOTSTRAP_BEGIN` only after `new` succeeds. Dropping this
/// value aborts the native capture and releases every engine allocation.
#[derive(Debug)]
pub struct NativeCheckpointCapture<'terminal> {
    capture: Option<Capture<'terminal, 'static>>,
    max_record_bytes: usize,
    finished: bool,
}

impl<'terminal> NativeCheckpointCapture<'terminal> {
    /// Preflight and begin a checkpoint using negotiated payload limits.
    ///
    /// Both negotiated axes constrain capture because active-state records and
    /// post-READY page records share libghostty's one strict record bound. The
    /// engine's own maxima are intersected as a final bound. No output is
    /// emitted during construction.
    pub fn new(
        terminal: &'terminal mut GhosttyTerminal<'_, '_>,
        limits: BootstrapLimits,
    ) -> Result<Self, NativeStateError> {
        let engine = incremental::capabilities()?;
        if !supports_protocol_07_native(&engine) {
            return Err(NativeStateError::UnsupportedFeature);
        }

        let chunk_bytes = usize::try_from(limits.max_chunk_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?;
        let history_bytes = usize::try_from(limits.max_history_page_bytes())
            .map_err(|_| NativeStateError::LimitExceeded)?;
        let max_record_bytes = chunk_bytes
            .min(history_bytes)
            .min(engine.max_record_bytes);
        if max_record_bytes == 0 {
            return Err(NativeStateError::LimitExceeded);
        }
        let max_pages = CaptureOptions::default().max_pages.min(engine.max_pages);
        if max_pages == 0 {
            return Err(NativeStateError::LimitExceeded);
        }

        // `capture` performs the load-bearing semantic-state preflight before
        // it returns: Kitty graphics, glyph glossary state, continuation
        // failures, limits, and OOM all fail before the host can publish BEGIN.
        let capture = terminal.capture(CaptureOptions {
            max_record_bytes,
            max_pages,
        })?;
        Ok(Self {
            capture: Some(capture),
            max_record_bytes,
            finished: false,
        })
    }

    /// Emit one complete opaque record directly into `buffer`.
    ///
    /// A zero or short buffer returns libghostty's exact `OutOfSpace` status,
    /// including `required_bytes`, without advancing. The buffer is never grown
    /// or retained by this host. Calling `step` after FINISH is an exact
    /// `InvalidState` error.
    pub fn step<'buffer>(
        &mut self,
        buffer: &'buffer mut [u8],
    ) -> Result<NativeCheckpointChunk<'buffer>, NativeStateError> {
        if self.finished {
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
                NativeCheckpointChunkKind::Ready { checkpoint }
            }
            CaptureEventKind::HistoryBegin { screen, count } => {
                NativeCheckpointChunkKind::HistoryBegin {
                    screen,
                    page_count: count,
                }
            }
            CaptureEventKind::HistoryPage {
                screen,
                index,
                count,
            } => NativeCheckpointChunkKind::HistoryPage {
                screen,
                index,
                page_count: count,
            },
            CaptureEventKind::Finish => {
                self.finished = true;
                NativeCheckpointChunkKind::Finish
            }
        };
        Ok(NativeCheckpointChunk {
            kind,
            codec_version: event.codec_version,
            bytes: event.record,
        })
    }

    /// Abort explicitly, releasing the terminal borrow and native resources.
    ///
    /// Dropping the capture provides the same cleanup when explicit status is
    /// not needed.
    pub fn abort(mut self) -> Result<(), NativeStateError> {
        self.capture
            .take()
            .ok_or(NativeStateError::InvalidState)?
            .abort()
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
        requested
            .max_history_page_bytes()
            .min(record_max)
            .min(unit_max),
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
        kitty
            .resize(10, 5, 8, 16)
            .expect("nonzero cell geometry");
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
    fn retries_short_buffers_without_advancing_and_orders_ready_before_history() {
        let mut source = terminal(20, 4);
        for row in 0..300 {
            source.vt_write(format!("history-{row:03}\r\n").as_bytes());
        }
        let limits = BootstrapLimits::new(64 * 1024, 64 * 1024).expect("test limits");
        let mut capture =
            NativeCheckpointCapture::new(&mut source, limits).expect("capture preflight");
        let mut ready = false;
        let mut history_pages = 0usize;

        loop {
            let required = match capture.step(&mut []) {
                Err(NativeStateError::OutOfSpace {
                    required_bytes,
                    required_rows: 0,
                }) => required_bytes,
                other => panic!("zero-buffer probe did not report exact size: {other:?}"),
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
            let event = capture.step(&mut exact).expect("exact record buffer");
            assert_eq!(event.bytes.len(), required);
            let finished = match event.kind {
                NativeCheckpointChunkKind::Ready { .. } => {
                    assert!(!ready, "READY is unique");
                    ready = true;
                    false
                }
                NativeCheckpointChunkKind::HistoryBegin { .. } => {
                    assert!(ready, "history begins only after READY");
                    false
                }
                NativeCheckpointChunkKind::HistoryPage { .. } => {
                    assert!(ready, "history pages follow READY");
                    assert!(
                        event.bytes.len()
                            <= usize::try_from(limits.max_history_page_bytes())
                                .expect("native usize")
                    );
                    history_pages += 1;
                    false
                }
                NativeCheckpointChunkKind::Finish => true,
                NativeCheckpointChunkKind::Record => false,
            };
            if finished {
                break;
            }
        }

        assert!(ready);
        assert!(history_pages > 0);
        assert_eq!(
            capture.step(&mut []).unwrap_err(),
            NativeStateError::InvalidState
        );
    }

    #[test]
    fn live_output_stays_raw_after_capture_and_abort_releases_terminal() {
        let mut source = terminal(20, 4);
        source.vt_write(b"checkpoint-state");
        let mut snapshot = Vec::new();
        {
            let mut capture = NativeCheckpointCapture::new(
                &mut source,
                BootstrapLimits::default(),
            )
            .expect("capture preflight");
            loop {
                let required = match capture.step(&mut []) {
                    Err(NativeStateError::OutOfSpace { required_bytes, .. }) => required_bytes,
                    other => panic!("record size probe: {other:?}"),
                };
                let mut record = vec![0; required];
                let event = capture.step(&mut record).expect("capture record");
                let finished = matches!(event.kind, NativeCheckpointChunkKind::Finish);
                snapshot.extend_from_slice(event.bytes);
                if finished {
                    break;
                }
            }
        }

        let raw = b"\r\nraw-after-finish";
        source.vt_write(raw);
        let mut replica = GhosttyTerminal::decode_snapshot(&snapshot)
            .expect("captured checkpoint")
            .terminal;
        replica.vt_write(raw);
        assert_eq!(
            source.encode_snapshot().expect("source state").as_ref(),
            replica.encode_snapshot().expect("replica state").as_ref()
        );

        let mut aborted = NativeCheckpointCapture::new(
            &mut source,
            BootstrapLimits::default(),
        )
        .expect("second capture");
        let _ = aborted.step(&mut []);
        aborted.abort().expect("explicit abort");
        source.vt_write(b"-after-abort");
        NativeCheckpointCapture::new(&mut source, BootstrapLimits::default())
            .expect("abort released canonical terminal");
    }

    #[test]
    fn failed_bounded_construction_cleans_up_and_preserves_native_status() {
        let mut source = terminal(20, 4);
        source.vt_write(b"\x1b[31mcontinuation and terminal state");
        let tiny = BootstrapLimits::new(1, 1).expect("protocol-valid tiny bounds");
        assert!(matches!(
            NativeCheckpointCapture::new(&mut source, tiny),
            Err(NativeStateError::LimitExceeded)
        ));
        source.vt_write(b"-still-live");
        NativeCheckpointCapture::new(&mut source, BootstrapLimits::default())
            .expect("failed native construction released all resources");
        assert_eq!(
            NativeStateError::OutOfMemory,
            incremental::Error::OutOfMemory,
            "OOM remains the engine's typed status"
        );
    }
}
