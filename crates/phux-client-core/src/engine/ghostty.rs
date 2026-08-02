//! Native libghostty implementation of the session-kernel engine boundary.
//!
//! The checkpoint stream remains opaque here. Fragmentation, authentication,
//! READY transfer, continuation replay, history retention, and FINISH are all
//! delegated to libghostty's safe incremental wrapper.

use std::{marker::PhantomData, rc::Rc};

use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use libghostty_vt::snapshot::incremental::{
    AfterReadyStep, DecodeProgress, DecodeStep, Decoder, DecoderOptions,
    Error as SnapshotError,
};
use phux_protocol::{
    BootstrapCapabilities, BootstrapLimits, BootstrapStreamProfile, EngineCodec,
    EngineFeatureSet,
};
use thiserror::Error;

use super::{
    BootstrapProgress, CanonicalGeometry, EngineAdapter, EngineDamage, EngineEffect,
    EngineEffectBuffer,
};

const CHECKPOINT_VERSION: u16 = EngineCodec::LibghosttyCheckpointV2 as u8 as u16;
const CHECKPOINT_IDENTITY: &str = "ghostty.snapshot.v1-v2.incremental.v1";
const SYNTH_SCROLLBACK_ROWS: usize = 10_000;

/// Return the client bootstrap capabilities supported by the linked engine.
///
/// Native v2 is added only when every required runtime guarantee and caller
/// bound is reported by libghostty. Any probe failure leaves both synthesized
/// compatibility profiles available.
#[must_use]
pub fn native_bootstrap_capabilities(limits: BootstrapLimits) -> BootstrapCapabilities {
    let capabilities = BootstrapCapabilities::new().with_limits(limits);
    let Ok(native) = libghostty_vt::snapshot::incremental::capabilities() else {
        return capabilities;
    };
    if supports_native(&native, limits) {
        capabilities.with_native(
            EngineCodec::LibghosttyCheckpointV2,
            EngineFeatureSet::required_native(),
        )
    } else {
        capabilities
    }
}

fn supports_native(
    capabilities: &libghostty_vt::snapshot::incremental::Capabilities,
    limits: BootstrapLimits,
) -> bool {
    let required_record_bytes = limits
        .max_chunk_bytes()
        .max(limits.max_history_page_bytes()) as usize;
    capabilities.default_encode_version == CHECKPOINT_VERSION
        && capabilities.min_decode_version <= CHECKPOINT_VERSION
        && capabilities.max_decode_version >= CHECKPOINT_VERSION
        && capabilities.incremental
        && capabilities.ready
        && capabilities.history
        && capabilities.authenticated_tokens
        && capabilities.bounded_records
        && capabilities.bounded_pages
        && capabilities.bounded_units
        && capabilities.max_record_bytes >= required_record_bytes
        && capabilities.max_pages > 0
        && capabilities.max_unit_bytes > 0
        && capabilities.max_rows > 0
        && capabilities.codec_identity == CHECKPOINT_IDENTITY
}

/// Concrete, current-thread libghostty engine host.
///
/// `limits` are the payload bounds negotiated in `HELLO_OK`; they become hard
/// decoder budgets rather than hints. The `Rc` marker deliberately keeps the
/// host on the same thread as every libghostty object it creates.
///
/// ```compile_fail
/// fn require_thread_safe<T: Send + Sync>() {}
/// require_thread_safe::<phux_client_core::engine::ghostty::GhosttyAdapter>();
/// ```
#[derive(Debug)]
pub struct GhosttyAdapter {
    limits: BootstrapLimits,
    decoder_options: DecoderOptions,
    native_available: bool,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl GhosttyAdapter {
    /// Construct an adapter for one connection's negotiated bootstrap limits.
    #[must_use]
    pub fn new(limits: BootstrapLimits) -> Self {
        let record_bytes = limits
            .max_chunk_bytes()
            .max(limits.max_history_page_bytes()) as usize;
        let defaults = DecoderOptions::default();
        let native = libghostty_vt::snapshot::incremental::capabilities()
            .ok()
            .filter(|capabilities| supports_native(capabilities, limits));
        let max_pages = native
            .as_ref()
            .map_or(defaults.max_pages, |capabilities| {
                defaults.max_pages.min(capabilities.max_pages)
            });
        Self {
            limits,
            decoder_options: DecoderOptions {
                max_continuation_bytes: defaults.max_continuation_bytes.min(record_bytes),
                max_record_bytes: record_bytes,
                max_pages,
            },
            native_available: native.is_some(),
            _not_send_or_sync: PhantomData,
        }
    }

    /// Negotiated payload limits enforced by this adapter.
    #[must_use]
    pub const fn limits(&self) -> BootstrapLimits {
        self.limits
    }
}

/// One adapter-owned libghostty replica.
///
/// ```compile_fail
/// fn require_thread_safe<T: Send + Sync>() {}
/// require_thread_safe::<phux_client_core::engine::ghostty::GhosttyReplica>();
/// ```
#[derive(Debug)]
pub struct GhosttyReplica {
    profile: BootstrapStreamProfile,
    state: ReplicaState,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl GhosttyReplica {
    /// Exact stream profile used to allocate this replica.
    #[must_use]
    pub const fn profile(&self) -> BootstrapStreamProfile {
        self.profile
    }

    /// Borrow the renderable terminal.
    ///
    /// Native replicas intentionally return `None` until authenticated READY
    /// has transferred the terminal and replayed its parser continuation.
    #[must_use]
    pub fn terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        match &self.state {
            ReplicaState::Synthesized { terminal, .. } => Some(terminal),
            ReplicaState::Native(native) => native.terminal(),
        }
    }
}

#[derive(Debug)]
enum ReplicaState {
    Synthesized {
        terminal: GhosttyTerminal<'static, 'static>,
        protocol_finished: bool,
    },
    Native(NativeReplica),
}

#[derive(Debug)]
struct NativeReplica {
    decoder: NativeDecoderState,
    protocol_finished: bool,
}

impl NativeReplica {
    fn terminal(&self) -> Option<&GhosttyTerminal<'static, 'static>> {
        match &self.decoder {
            NativeDecoderState::BeforeReady(_) | NativeDecoderState::Failed(None) => None,
            NativeDecoderState::AfterReady(stream) => Some(stream.terminal()),
            NativeDecoderState::Finished(terminal) | NativeDecoderState::Failed(Some(terminal)) => {
                Some(terminal)
            }
        }
    }
}

#[derive(Debug)]
enum NativeDecoderState {
    BeforeReady(Decoder<'static>),
    AfterReady(libghostty_vt::snapshot::incremental::DecodedStream<'static, 'static>),
    Finished(GhosttyTerminal<'static, 'static>),
    Failed(Option<GhosttyTerminal<'static, 'static>>),
}

/// Typed failures from the concrete libghostty engine host.
#[derive(Debug, Error)]
pub enum GhosttyEngineError {
    /// The selected stream profile is unavailable from the linked engine.
    #[error("unsupported bootstrap stream profile: {0:?}")]
    UnsupportedProfile(BootstrapStreamProfile),
    /// A normal terminal allocation or query failed.
    #[error("libghostty terminal operation failed: {0}")]
    Terminal(#[from] libghostty_vt::Error),
    /// The incremental checkpoint wrapper rejected the opaque stream.
    #[error("libghostty checkpoint failed after consuming {consumed} bytes: {source}")]
    Checkpoint {
        /// Exact libghostty status; callers never recover it from text.
        source: SnapshotError,
        /// Exact bytes consumed from the submitted fragment.
        consumed: usize,
    },
    /// An envelope for a different exact native codec was received.
    #[error("wrong checkpoint codec version: expected {expected}, got {actual}")]
    WrongCodecVersion {
        /// Negotiated immutable codec version.
        expected: u16,
        /// Version authenticated by the decoder.
        actual: u16,
    },
    /// One borrowed protocol payload exceeded its negotiated frame bound.
    #[error("engine payload is {actual} bytes; negotiated limit is {limit}")]
    PayloadLimitExceeded {
        /// Borrowed payload length.
        actual: usize,
        /// Negotiated maximum payload length.
        limit: usize,
    },
    /// Compatibility profiles do not accept native history pages.
    #[error("history pages are unsupported for bootstrap stream profile: {0:?}")]
    HistoryUnsupported(BootstrapStreamProfile),
    /// A native bootstrap chunk continued past its authenticated READY record.
    #[error("{trailing} trailing bootstrap bytes after READY")]
    TrailingAfterReady {
        /// Unconsumed bytes after authenticated READY.
        trailing: usize,
    },
    /// A bootstrap chunk arrived after authenticated READY.
    #[error("bootstrap input arrived after READY")]
    InputAfterReady,
    /// A history page arrived before protocol READY published the replica.
    #[error("history page arrived before the native replica was published")]
    HistoryBeforePublication,
    /// Live bytes arrived before the native READY transfer completed.
    #[error("native terminal is not ready for live output")]
    LiveOutputBeforeReady,
    /// Bootstrap bytes or a second protocol finish arrived after FINISH.
    #[error("bootstrap input arrived after FINISH")]
    InputAfterFinish,
    /// FINISH was followed by bytes in the same borrowed fragment.
    #[error("{trailing} trailing bootstrap bytes after FINISH")]
    TrailingAfterFinish {
        /// Unconsumed bytes after the authenticated FINISH record.
        trailing: usize,
    },
    /// The wrapper reported transition accounting that cannot make progress.
    #[error("invalid checkpoint transition accounting: consumed {consumed} of {available}")]
    InvalidProgress {
        /// Wrapper-reported byte consumption.
        consumed: usize,
        /// Bytes offered in this transition.
        available: usize,
    },
    /// The native decoder has already failed and cannot be driven again.
    #[error("native checkpoint decoder is no longer usable")]
    DecoderFailed,
}

impl GhosttyEngineError {
    fn checkpoint(source: SnapshotError, consumed: usize) -> Self {
        Self::Checkpoint { source, consumed }
    }
}

impl EngineAdapter for GhosttyAdapter {
    type Replica = GhosttyReplica;
    type Error = GhosttyEngineError;

    fn start_replica(
        &mut self,
        profile: BootstrapStreamProfile,
        geometry: CanonicalGeometry,
    ) -> Result<Self::Replica, Self::Error> {
        let state = match profile {
            BootstrapStreamProfile::SynthesizedVtRaw
            | BootstrapStreamProfile::SynthesizedVtStateSync => ReplicaState::Synthesized {
                terminal: GhosttyTerminal::new(TerminalOptions {
                    cols: geometry.cols,
                    rows: geometry.rows,
                    max_scrollback: SYNTH_SCROLLBACK_ROWS,
                })?,
                protocol_finished: false,
            },
            BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttyCheckpointV2,
            } if self.native_available => {
                let decoder = Decoder::new(self.decoder_options)
                    .map_err(|error| GhosttyEngineError::checkpoint(error, 0))?;
                ReplicaState::Native(NativeReplica {
                    decoder: NativeDecoderState::BeforeReady(decoder),
                    protocol_finished: false,
                })
            }
            _ => return Err(GhosttyEngineError::UnsupportedProfile(profile)),
        };
        Ok(GhosttyReplica {
            profile,
            state,
            _not_send_or_sync: PhantomData,
        })
    }

    fn apply_bootstrap_chunk(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        let limit = self.limits.max_chunk_bytes() as usize;
        if payload.len() > limit {
            return Err(GhosttyEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        match &mut replica.state {
            ReplicaState::Synthesized {
                terminal,
                protocol_finished,
            } => {
                if *protocol_finished {
                    return Err(GhosttyEngineError::InputAfterFinish);
                }
                terminal.vt_write(payload);
                Ok(BootstrapProgress::Pending)
            }
            ReplicaState::Native(native) => push_native(native, payload),
        }
    }

    fn finish_bootstrap(
        &mut self,
        replica: &mut Self::Replica,
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        match &mut replica.state {
            ReplicaState::Synthesized {
                protocol_finished, ..
            } => {
                if std::mem::replace(protocol_finished, true) {
                    return Err(GhosttyEngineError::InputAfterFinish);
                }
                Ok(BootstrapProgress::Finished)
            }
            ReplicaState::Native(native) => finish_native(native),
        }
    }

    fn apply_history_page(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        _effects: &mut EngineEffectBuffer,
    ) -> Result<BootstrapProgress, Self::Error> {
        let limit = self.limits.max_history_page_bytes() as usize;
        if payload.len() > limit {
            return Err(GhosttyEngineError::PayloadLimitExceeded {
                actual: payload.len(),
                limit,
            });
        }
        let profile = replica.profile;
        match &mut replica.state {
            ReplicaState::Synthesized { .. } => {
                Err(GhosttyEngineError::HistoryUnsupported(profile))
            }
            ReplicaState::Native(native) => push_history(native, payload),
        }
    }

    fn apply_output(
        &mut self,
        replica: &mut Self::Replica,
        payload: &[u8],
        effects: &mut EngineEffectBuffer,
    ) -> Result<(), Self::Error> {
        match &mut replica.state {
            ReplicaState::Native(native) if !native.protocol_finished => {
                return Err(GhosttyEngineError::LiveOutputBeforeReady);
            }
            ReplicaState::Synthesized { terminal, .. } => terminal.vt_write(payload),
            ReplicaState::Native(native) => match &mut native.decoder {
                NativeDecoderState::BeforeReady(_) => {
                    return Err(GhosttyEngineError::LiveOutputBeforeReady);
                }
                NativeDecoderState::AfterReady(stream) => stream.terminal_mut().vt_write(payload),
                NativeDecoderState::Finished(terminal)
                | NativeDecoderState::Failed(Some(terminal)) => terminal.vt_write(payload),
                NativeDecoderState::Failed(None) => {
                    return Err(GhosttyEngineError::DecoderFailed);
                }
            },
        }
        effects.push(EngineEffect::Damage(EngineDamage::Full));
        Ok(())
    }
}

fn push_native(
    native: &mut NativeReplica,
    mut input: &[u8],
) -> Result<BootstrapProgress, GhosttyEngineError> {
    if native.protocol_finished {
        return Err(GhosttyEngineError::InputAfterReady);
    }
    if input.is_empty() {
        return match native.decoder {
            NativeDecoderState::BeforeReady(_) => Ok(BootstrapProgress::Pending),
            NativeDecoderState::AfterReady(_) => Err(GhosttyEngineError::InputAfterReady),
            NativeDecoderState::Finished(_) => Err(GhosttyEngineError::InputAfterFinish),
            NativeDecoderState::Failed(_) => Err(GhosttyEngineError::DecoderFailed),
        };
    }

    loop {
        let state = std::mem::replace(&mut native.decoder, NativeDecoderState::Failed(None));
        match state {
            NativeDecoderState::BeforeReady(decoder) => match decoder.push(input) {
                Err(failure) => {
                    return Err(GhosttyEngineError::checkpoint(
                        failure.error,
                        failure.consumed,
                    ));
                }
                Ok(DecodeStep::NeedInput { decoder, progress })
                | Ok(DecodeStep::Progress { decoder, progress }) => {
                    check_version(progress)?;
                    native.decoder = NativeDecoderState::BeforeReady(decoder);
                    input = remaining(input, progress)?;
                    if input.is_empty() {
                        return Ok(BootstrapProgress::Pending);
                    }
                }
                Ok(DecodeStep::Ready { decoder, progress }) => {
                    check_version(progress)?;
                    let continuation = decoder
                        .take_terminal()
                        .map_err(|error| GhosttyEngineError::checkpoint(error, progress.consumed))?;
                    let stream = continuation.replay().map_err(|failure| {
                        GhosttyEngineError::checkpoint(failure.error, progress.consumed)
                    })?;
                    let trailing_result =
                        remaining(input, progress).map(|remaining| remaining.len());
                    native.decoder = NativeDecoderState::AfterReady(stream);
                    let trailing = trailing_result?;
                    if trailing != 0 {
                        return Err(GhosttyEngineError::TrailingAfterReady { trailing });
                    }
                    return Ok(BootstrapProgress::Ready);
                }
            },
            NativeDecoderState::AfterReady(stream) => {
                native.decoder = NativeDecoderState::AfterReady(stream);
                return Err(GhosttyEngineError::InputAfterReady);
            }
            NativeDecoderState::Finished(terminal) => {
                native.decoder = NativeDecoderState::Finished(terminal);
                return Err(GhosttyEngineError::InputAfterFinish);
            }
            NativeDecoderState::Failed(terminal) => {
                native.decoder = NativeDecoderState::Failed(terminal);
                return Err(GhosttyEngineError::DecoderFailed);
            }
        }
    }
}

fn finish_native(native: &mut NativeReplica) -> Result<BootstrapProgress, GhosttyEngineError> {
    if native.protocol_finished {
        return Err(GhosttyEngineError::InputAfterFinish);
    }
    let state = std::mem::replace(&mut native.decoder, NativeDecoderState::Failed(None));
    match state {
        NativeDecoderState::BeforeReady(decoder) => match decoder.end_input() {
            Err(failure) => Err(GhosttyEngineError::checkpoint(
                failure.error,
                failure.consumed,
            )),
            Ok(_) => Err(GhosttyEngineError::checkpoint(
                SnapshotError::InvalidState,
                0,
            )),
        },
        NativeDecoderState::AfterReady(stream) => {
            native.decoder = NativeDecoderState::AfterReady(stream);
            native.protocol_finished = true;
            Ok(BootstrapProgress::Finished)
        }
        NativeDecoderState::Finished(terminal) => {
            native.decoder = NativeDecoderState::Finished(terminal);
            Err(GhosttyEngineError::InputAfterFinish)
        }
        NativeDecoderState::Failed(terminal) => {
            native.decoder = NativeDecoderState::Failed(terminal);
            Err(GhosttyEngineError::DecoderFailed)
        }
    }
}

fn push_history(
    native: &mut NativeReplica,
    mut input: &[u8],
) -> Result<BootstrapProgress, GhosttyEngineError> {
    if !native.protocol_finished {
        return Err(GhosttyEngineError::HistoryBeforePublication);
    }
    if input.is_empty() {
        return match native.decoder {
            NativeDecoderState::AfterReady(_) => Ok(BootstrapProgress::Ready),
            NativeDecoderState::Finished(_) => Err(GhosttyEngineError::InputAfterFinish),
            NativeDecoderState::BeforeReady(_) => Err(GhosttyEngineError::HistoryBeforePublication),
            NativeDecoderState::Failed(_) => Err(GhosttyEngineError::DecoderFailed),
        };
    }

    loop {
        let state = std::mem::replace(&mut native.decoder, NativeDecoderState::Failed(None));
        let stream = match state {
            NativeDecoderState::AfterReady(stream) => stream,
            NativeDecoderState::Finished(terminal) => {
                native.decoder = NativeDecoderState::Finished(terminal);
                return Err(GhosttyEngineError::InputAfterFinish);
            }
            NativeDecoderState::BeforeReady(decoder) => {
                native.decoder = NativeDecoderState::BeforeReady(decoder);
                return Err(GhosttyEngineError::HistoryBeforePublication);
            }
            NativeDecoderState::Failed(terminal) => {
                native.decoder = NativeDecoderState::Failed(terminal);
                return Err(GhosttyEngineError::DecoderFailed);
            }
        };
        match stream.push(input) {
            Err(failure) => {
                native.decoder = NativeDecoderState::Failed(Some(failure.terminal));
                return Err(GhosttyEngineError::checkpoint(
                    failure.error,
                    failure.consumed,
                ));
            }
            Ok(AfterReadyStep::NeedInput { decoder, progress })
            | Ok(AfterReadyStep::Progress { decoder, progress })
            | Ok(AfterReadyStep::HistoryBegin {
                decoder, progress, ..
            })
            | Ok(AfterReadyStep::HistoryPage {
                decoder, progress, ..
            }) => {
                check_version(progress)?;
                native.decoder = NativeDecoderState::AfterReady(decoder);
                input = remaining(input, progress)?;
                if input.is_empty() {
                    return Ok(BootstrapProgress::Ready);
                }
            }
            Ok(AfterReadyStep::Finish(finished)) => {
                check_version(finished.progress)?;
                if finished.codec_version != CHECKPOINT_VERSION {
                    native.decoder = NativeDecoderState::Finished(finished.terminal);
                    return Err(GhosttyEngineError::WrongCodecVersion {
                        expected: CHECKPOINT_VERSION,
                        actual: finished.codec_version,
                    });
                }
                let trailing_result =
                    remaining(input, finished.progress).map(|remaining| remaining.len());
                native.decoder = NativeDecoderState::Finished(finished.terminal);
                let trailing = trailing_result?;
                if trailing != 0 {
                    return Err(GhosttyEngineError::TrailingAfterFinish { trailing });
                }
                return Ok(BootstrapProgress::Finished);
            }
        }
    }
}

fn check_version(progress: DecodeProgress) -> Result<(), GhosttyEngineError> {
    if progress.codec_version != 0 && progress.codec_version != CHECKPOINT_VERSION {
        return Err(GhosttyEngineError::WrongCodecVersion {
            expected: CHECKPOINT_VERSION,
            actual: progress.codec_version,
        });
    }
    Ok(())
}

fn remaining<'a>(
    input: &'a [u8],
    progress: DecodeProgress,
) -> Result<&'a [u8], GhosttyEngineError> {
    if progress.consumed == 0 || progress.consumed > input.len() {
        return Err(GhosttyEngineError::InvalidProgress {
            consumed: progress.consumed,
            available: input.len(),
        });
    }
    Ok(&input[progress.consumed..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::snapshot::incremental::{
        CaptureEventKind, CaptureOptions, Error as SnapshotError,
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RecordKind {
        Other,
        Ready,
        History,
        Finish,
    }

    fn geometry() -> CanonicalGeometry {
        CanonicalGeometry::new(80, 4).expect("valid geometry")
    }

    fn native_profile() -> BootstrapStreamProfile {
        BootstrapStreamProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
        }
    }

    fn capture_records() -> Vec<(RecordKind, Vec<u8>)> {
        let mut source = GhosttyTerminal::new(TerminalOptions {
            cols: 80,
            rows: 4,
            max_scrollback: 100,
        })
        .expect("source terminal");
        for line in 0..40 {
            source.vt_write(format!("line {line}\r\n").as_bytes());
        }
        source.vt_write(b"\x1b]2;checkpoint-title\x07");

        let mut capture = source
            .capture(CaptureOptions::default())
            .expect("incremental capture");
        let mut records = Vec::new();
        loop {
            let required = match capture.next(&mut []) {
                Err(SnapshotError::OutOfSpace {
                    required_bytes,
                    required_rows: 0,
                }) => required_bytes,
                other => panic!("capture size probe: {other:?}"),
            };
            let mut bytes = vec![0; required];
            let event = capture.next(&mut bytes).expect("capture record");
            let kind = match event.kind {
                CaptureEventKind::Ready { .. } => RecordKind::Ready,
                CaptureEventKind::HistoryBegin { .. }
                | CaptureEventKind::HistoryPage { .. } => RecordKind::History,
                CaptureEventKind::Finish => RecordKind::Finish,
                CaptureEventKind::Record => RecordKind::Other,
            };
            records.push((kind, bytes));
            if kind == RecordKind::Finish {
                return records;
            }
        }
    }

    fn native_adapter() -> GhosttyAdapter {
        GhosttyAdapter::new(BootstrapLimits::default())
    }

    fn split_capture(records: &[(RecordKind, Vec<u8>)]) -> (Vec<u8>, Vec<u8>) {
        let ready = records
            .iter()
            .position(|(kind, _)| *kind == RecordKind::Ready)
            .expect("READY record");
        let bootstrap = records[..=ready]
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect();
        let history = records[ready + 1..]
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect();
        (bootstrap, history)
    }

    #[test]
    fn synthesized_profiles_write_borrowed_bytes_and_reject_history() {
        for profile in [
            BootstrapStreamProfile::SynthesizedVtRaw,
            BootstrapStreamProfile::SynthesizedVtStateSync,
        ] {
            let mut adapter = native_adapter();
            let mut replica = adapter
                .start_replica(profile, geometry())
                .expect("synth replica");
            let mut effects = EngineEffectBuffer::new();
            assert_eq!(
                adapter
                    .apply_bootstrap_chunk(&mut replica, b"\x1b]2;synth-title\x07", &mut effects)
                    .expect("bootstrap bytes"),
                BootstrapProgress::Pending
            );
            assert_eq!(
                adapter
                    .finish_bootstrap(&mut replica, &mut effects)
                    .expect("protocol finish"),
                BootstrapProgress::Finished
            );
            assert_eq!(
                replica.terminal().expect("synth terminal").title().unwrap(),
                "synth-title"
            );
            assert!(matches!(
                adapter.apply_history_page(&mut replica, b"history", &mut effects),
                Err(GhosttyEngineError::HistoryUnsupported(actual)) if actual == profile
            ));
            assert!(matches!(
                adapter.apply_bootstrap_chunk(&mut replica, b"late", &mut effects),
                Err(GhosttyEngineError::InputAfterFinish)
            ));
        }
    }

    #[test]
    fn native_decoder_accepts_arbitrary_fragment_cuts_and_multiple_records() {
        let records = capture_records();
        let (bootstrap, history) = split_capture(&records);
        for width in [1, 2, 3, 7, 31, bootstrap.len().max(history.len())] {
            let mut adapter = native_adapter();
            let mut replica = adapter
                .start_replica(native_profile(), geometry())
                .expect("native replica");
            let mut effects = EngineEffectBuffer::new();
            let mut bootstrap_progress = BootstrapProgress::Pending;
            for fragment in bootstrap.chunks(width) {
                bootstrap_progress = adapter
                    .apply_bootstrap_chunk(&mut replica, fragment, &mut effects)
                    .expect("arbitrary bootstrap fragment");
            }
            assert_eq!(bootstrap_progress, BootstrapProgress::Ready);
            assert_eq!(
                adapter
                    .finish_bootstrap(&mut replica, &mut effects)
                    .expect("protocol READY"),
                BootstrapProgress::Finished
            );
            let mut history_progress = BootstrapProgress::Ready;
            for fragment in history.chunks(width) {
                history_progress = adapter
                    .apply_history_page(&mut replica, fragment, &mut effects)
                    .expect("arbitrary history fragment");
            }
            assert_eq!(history_progress, BootstrapProgress::Finished);
            assert_eq!(
                replica.terminal().expect("finished terminal").title().unwrap(),
                "checkpoint-title"
            );
        }
    }

    #[test]
    fn publication_requires_authenticated_ready_and_one_shot_continuation_replay() {
        let records = capture_records();
        let ready = records
            .iter()
            .position(|(kind, _)| *kind == RecordKind::Ready)
            .expect("READY record");
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        assert!(matches!(
            adapter.apply_output(&mut replica, b"too early", &mut effects),
            Err(GhosttyEngineError::LiveOutputBeforeReady)
        ));
        for (_, record) in &records[..ready] {
            assert_eq!(
                adapter
                    .apply_bootstrap_chunk(&mut replica, record, &mut effects)
                    .expect("pre-READY record"),
                BootstrapProgress::Pending
            );
            assert!(replica.terminal().is_none());
        }
        assert_eq!(
            adapter
                .apply_bootstrap_chunk(&mut replica, &records[ready].1, &mut effects)
                .expect("READY record"),
            BootstrapProgress::Ready
        );
        assert!(replica.terminal().is_some());
        assert!(matches!(
            adapter.apply_output(&mut replica, b"not published", &mut effects),
            Err(GhosttyEngineError::LiveOutputBeforeReady)
        ));
        assert!(matches!(
            adapter.apply_bootstrap_chunk(&mut replica, b"late chunk", &mut effects),
            Err(GhosttyEngineError::InputAfterReady)
        ));
        assert_eq!(
            adapter
                .finish_bootstrap(&mut replica, &mut effects)
                .expect("protocol READY validates native READY"),
            BootstrapProgress::Finished
        );
        adapter
            .apply_output(&mut replica, b"\x1b]2;published-live\x07", &mut effects)
            .expect("published live output");
        assert_eq!(replica.terminal().unwrap().title().unwrap(), "published-live");
    }

    #[test]
    fn live_output_is_applied_between_later_history_pages() {
        let records = capture_records();
        let ready = records
            .iter()
            .position(|(kind, _)| *kind == RecordKind::Ready)
            .expect("READY record");
        assert!(records.iter().any(|(kind, _)| *kind == RecordKind::History));
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        for (_, record) in &records[..=ready] {
            adapter
                .apply_bootstrap_chunk(&mut replica, record, &mut effects)
                .expect("checkpoint bootstrap record");
        }
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .expect("protocol READY");

        let mut wrote_live = false;
        for (kind, record) in &records[ready + 1..] {
            adapter
                .apply_history_page(&mut replica, record, &mut effects)
                .expect("history page");
            if !wrote_live && *kind == RecordKind::History {
                adapter
                    .apply_output(
                        &mut replica,
                        b"\x1b]2;live-during-history\x07",
                        &mut effects,
                    )
                    .expect("live output between history pages");
                wrote_live = true;
            }
        }
        assert!(wrote_live);
        assert_eq!(
            replica.terminal().expect("finished terminal").title().unwrap(),
            "live-during-history"
        );
    }

    #[test]
    fn native_truncation_corruption_and_limits_are_typed() {
        let records = capture_records();
        let (bootstrap, _) = split_capture(&records);
        let mut effects = EngineEffectBuffer::new();

        let mut adapter = native_adapter();
        let mut early = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        assert!(matches!(
            adapter.finish_bootstrap(&mut early, &mut effects),
            Err(GhosttyEngineError::Checkpoint {
                source: SnapshotError::Truncated,
                ..
            })
        ));

        let mut adapter = native_adapter();
        let mut truncated = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        adapter
            .apply_bootstrap_chunk(
                &mut truncated,
                &bootstrap[..bootstrap.len() - 1],
                &mut effects,
            )
            .expect("truncated fragment is buffered");
        assert!(matches!(
            adapter.finish_bootstrap(&mut truncated, &mut effects),
            Err(GhosttyEngineError::Checkpoint {
                source: SnapshotError::Truncated,
                ..
            })
        ));

        let mut corrupt = bootstrap.clone();
        corrupt[0] ^= 0xff;
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        assert!(matches!(
            adapter.apply_bootstrap_chunk(&mut replica, &corrupt, &mut effects),
            Err(GhosttyEngineError::Checkpoint { .. })
                | Err(GhosttyEngineError::WrongCodecVersion { .. })
        ));

        let tiny = BootstrapLimits::new(1, 1).expect("tiny valid limits");
        let mut adapter = GhosttyAdapter::new(tiny);
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        assert!(matches!(
            adapter.apply_bootstrap_chunk(&mut replica, &bootstrap, &mut effects),
            Err(GhosttyEngineError::PayloadLimitExceeded { limit: 1, .. })
        ));

        let mut adapter = GhosttyAdapter::new(tiny);
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut limit_error = None;
        for byte in &bootstrap {
            if let Err(error) =
                adapter.apply_bootstrap_chunk(&mut replica, std::slice::from_ref(byte), &mut effects)
            {
                limit_error = Some(error);
                break;
            }
        }
        assert!(matches!(
            limit_error,
            Some(GhosttyEngineError::Checkpoint {
                source: SnapshotError::LimitExceeded,
                ..
            })
        ));
    }

    #[test]
    fn native_history_finish_rejects_trailing_and_post_finish_pages() {
        let records = capture_records();
        let (bootstrap, mut history) = split_capture(&records);
        history.extend_from_slice(b"trailing");
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        adapter
            .apply_bootstrap_chunk(&mut replica, &bootstrap, &mut effects)
            .expect("checkpoint through READY");
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .expect("protocol READY");
        assert!(matches!(
            adapter.apply_history_page(&mut replica, &history, &mut effects),
            Err(GhosttyEngineError::TrailingAfterFinish { trailing: 8 })
        ));
        assert!(matches!(
            adapter.apply_history_page(&mut replica, b"again", &mut effects),
            Err(GhosttyEngineError::InputAfterFinish)
        ));
    }

    #[test]
    fn native_profile_is_rejected_when_the_runtime_contract_is_unavailable() {
        let mut adapter = native_adapter();
        adapter.native_available = false;
        assert!(matches!(
            adapter.start_replica(native_profile(), geometry()),
            Err(GhosttyEngineError::UnsupportedProfile(
                BootstrapStreamProfile::NativeState {
                    codec: EngineCodec::LibghosttyCheckpointV2,
                }
            ))
        ));
    }

    #[test]
    fn native_capabilities_require_the_exact_runtime_contract() {
        let limits = BootstrapLimits::default();
        let capabilities = native_bootstrap_capabilities(limits);
        assert!(capabilities
            .native_codecs
            .contains(EngineCodec::LibghosttyCheckpointV2));
        assert_eq!(
            capabilities.native_features,
            EngineFeatureSet::required_native()
        );
        assert_eq!(capabilities.limits, limits);
    }
}
