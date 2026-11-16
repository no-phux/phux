//! Native snapshot and history decoding, plus live effect hooks.

use super::{
    BootstrapProgress, CONTINUATION_LIMIT, Cell, ClipboardWrite, FeedDecoder, GhosttyEngineError,
    GhosttyTerminal, HistoryApplyOutcome, NativeDecoderState, NativeReplica, Rc, RefCell,
};
use libghostty_vt::snapshot::{FeedIncrementalDecoder, FeedProgress, ProgressKind};
use phux_protocol::snapshot_framing::{HistoryFraming, HistoryUnit};

pub(super) fn start_native(
    pending: Rc<PendingEffects>,
) -> Result<NativeReplica, GhosttyEngineError> {
    // A bare PAGE gets a fixed-size, reader-local empty placement record.
    let mut decoder = FeedDecoder::new(CONTINUATION_LIMIT + 16)?;
    decoder.set_max_continuation_bytes(CONTINUATION_LIMIT)?;
    // The stock decoder otherwise restores parser state but stops tracking it.
    // Retain it from READY so Clear can export even the captured partial input.
    decoder.set_retain_continuation(true)?;
    Ok(NativeReplica {
        pending,
        decoder: NativeDecoderState::Collecting(decoder),
        protocol_finished: false,
        history_framing: HistoryFraming::default(),
        history_failed: false,
    })
}

pub(super) fn push_native(
    native: &mut NativeReplica,
    input: &[u8],
) -> Result<BootstrapProgress, GhosttyEngineError> {
    if native.protocol_finished {
        return Err(GhosttyEngineError::InputAfterReady);
    }
    let NativeDecoderState::Collecting(decoder) = &mut native.decoder else {
        return match native.decoder {
            NativeDecoderState::Ready(_) => Err(GhosttyEngineError::InputAfterReady),
            NativeDecoderState::Finished(_) => Err(GhosttyEngineError::InputAfterFinish),
            NativeDecoderState::Failed => Err(GhosttyEngineError::DecoderFailed),
            NativeDecoderState::Collecting(_) => unreachable!(),
        };
    };
    if decoder.staged_bytes().saturating_add(input.len()) > CONTINUATION_LIMIT {
        return Err(GhosttyEngineError::checkpoint(
            libghostty_vt::Error::LimitExceeded,
            0,
        ));
    }
    decoder
        .feed(input)
        .map_err(|error| GhosttyEngineError::checkpoint(error, 0))?;
    Ok(BootstrapProgress::Pending)
}

fn attach_native_callbacks(
    native: &NativeReplica,
    terminal: &mut GhosttyTerminal<'static, 'static>,
) -> Result<(), GhosttyEngineError> {
    phux_vt::kitty_replay::configure_terminal_for_kitty_graphics(terminal)?;
    if terminal.width_px()? == 0 || terminal.height_px()? == 0 {
        terminal.resize(terminal.cols()?, terminal.rows()?, 8, 16)?;
    }
    install_effect_hooks(terminal, &native.pending)
}

/// The largest OSC 52 payload a replica passes on (ADR-0158).
const MAX_CLIPBOARD_BYTES: usize = 1 << 20;

/// Effects a replica's libghostty callbacks raise inside `vt_write`.
/// `apply_output` drains them as engine effects; bootstrap drops them, since
/// replayed history is not new activity.
#[derive(Debug, Default)]
pub(super) struct PendingEffects {
    pub(super) bell: Cell<bool>,
    /// The latest clipboard write; an earlier one in the same chunk loses.
    pub(super) clipboard: RefCell<Option<String>>,
    /// Finds OSC 52 reads in live output across chunks.
    pub(super) clipboard_reads: RefCell<crate::engine::osc52_read::Osc52ReadScanner>,
}

impl PendingEffects {
    pub(super) fn clear(&self) {
        self.bell.set(false);
        self.clipboard.take();
        self.clipboard_reads.take();
    }
}

pub(super) fn install_effect_hooks(
    terminal: &mut GhosttyTerminal<'static, 'static>,
    pending: &Rc<PendingEffects>,
) -> Result<(), GhosttyEngineError> {
    terminal.on_bell({
        let pending = Rc::clone(pending);
        move |_terminal| pending.bell.set(true)
    })?;
    terminal.on_clipboard_write({
        let pending = Rc::clone(pending);
        move |_terminal, write| {
            if let Some(text) = clipboard_text(&write) {
                pending.clipboard.replace(Some(text));
            }
            write.reply(Ok(()), false);
        }
    })?;
    Ok(())
}

/// The UTF-8 `text/plain` representation of a clipboard write, or `None` for
/// a clear, a non-text write, or one over [`MAX_CLIPBOARD_BYTES`]. Every
/// destination (standard, selection, primary) means the host clipboard.
fn clipboard_text(write: &ClipboardWrite<'_>) -> Option<String> {
    let content = write
        .contents()
        .find(|content| content.mime.starts_with(b"text/plain"))?;
    if content.data.len() > MAX_CLIPBOARD_BYTES {
        return None;
    }
    String::from_utf8(content.data.to_vec()).ok()
}

pub(super) fn finish_native(
    native: &mut NativeReplica,
) -> Result<BootstrapProgress, GhosttyEngineError> {
    if native.protocol_finished {
        return Err(GhosttyEngineError::InputAfterFinish);
    }
    let state = std::mem::replace(&mut native.decoder, NativeDecoderState::Failed);
    let NativeDecoderState::Collecting(decoder) = state else {
        return Err(match state {
            NativeDecoderState::Ready(_) | NativeDecoderState::Finished(_) => {
                GhosttyEngineError::InputAfterFinish
            }
            NativeDecoderState::Failed => GhosttyEngineError::DecoderFailed,
            NativeDecoderState::Collecting(_) => unreachable!(),
        });
    };
    let staged = decoder.staged_bytes();
    let mut decoder = decoder
        .ready()
        .map_err(|error| GhosttyEngineError::checkpoint(error, staged))?;
    require_consumed_unit(&decoder, staged, staged)?;
    attach_native_callbacks(native, decoder.terminal_mut())?;
    native.decoder = NativeDecoderState::Ready(decoder);
    native.protocol_finished = true;
    Ok(BootstrapProgress::Ready)
}

pub(super) fn push_history(
    native: &mut NativeReplica,
    input: &[u8],
    declared_rows: u32,
) -> Result<HistoryApplyOutcome, GhosttyEngineError> {
    if !native.protocol_finished {
        return Err(GhosttyEngineError::HistoryBeforePublication);
    }
    if native.history_failed {
        return Err(GhosttyEngineError::DecoderFailed);
    }
    let result = decode_history_unit(native, input, declared_rows);
    if result.is_err() {
        fail_history_decoder(native);
    }
    result
}

fn decode_history_unit(
    native: &mut NativeReplica,
    input: &[u8],
    declared_rows: u32,
) -> Result<HistoryApplyOutcome, GhosttyEngineError> {
    let NativeDecoderState::Ready(decoder) = &mut native.decoder else {
        return Err(match native.decoder {
            NativeDecoderState::Finished(_) => GhosttyEngineError::InputAfterFinish,
            NativeDecoderState::Failed => GhosttyEngineError::DecoderFailed,
            NativeDecoderState::Collecting(_) | NativeDecoderState::Ready(_) => {
                GhosttyEngineError::HistoryBeforePublication
            }
        });
    };
    let invalid = || GhosttyEngineError::checkpoint(libghostty_vt::Error::InvalidValue, 0);
    // All original bytes, including unknown/placement suffixes, are checked
    // before any engine mutation. Row declarations never come from the peer's
    // unauthenticated outer metadata.
    let (unit, suffix) = native.history_framing.prepare(input).ok_or_else(invalid)?;
    let authenticated_rows = unit.rows();
    if u32::try_from(authenticated_rows).ok() != Some(declared_rows) {
        return Err(invalid());
    }
    let padding = suffix.as_ref().map_or(&[][..], |bytes| &bytes[..]);
    let progress = decode_staged_history(decoder, input, padding)?;
    let outcome = history_outcome(unit, progress)?;
    if outcome.progress == BootstrapProgress::Finished {
        return Ok(finish_history_decoder(native));
    }
    Ok(outcome)
}

const fn history_outcome(
    unit: HistoryUnit,
    progress: Option<FeedProgress>,
) -> Result<HistoryApplyOutcome, GhosttyEngineError> {
    let retained_rows = match (unit, progress) {
        (HistoryUnit::Finish, None) => {
            return Ok(HistoryApplyOutcome {
                progress: BootstrapProgress::Finished,
                retained_rows: 0,
                authenticated_rows: 0,
            });
        }
        (
            HistoryUnit::Page { .. },
            Some(FeedProgress {
                kind: ProgressKind::Page { .. },
                rows,
                ..
            }),
        ) => rows,
        (
            HistoryUnit::Image,
            Some(FeedProgress {
                kind: ProgressKind::Image { .. },
                ..
            }),
        ) => 0,
        _ => {
            return Err(GhosttyEngineError::checkpoint(
                libghostty_vt::Error::InvalidValue,
                0,
            ));
        }
    };
    Ok(HistoryApplyOutcome {
        progress: BootstrapProgress::Ready,
        retained_rows,
        authenticated_rows: unit.rows(),
    })
}

fn decode_staged_history(
    decoder: &mut FeedIncrementalDecoder<'static, 'static>,
    input: &[u8],
    suffix: &[u8],
) -> Result<Option<FeedProgress>, GhosttyEngineError> {
    decoder
        .feed_parts(input, suffix)
        .map_err(|error| GhosttyEngineError::checkpoint(error, 0))?;
    let staged = decoder.staged_bytes();
    let progress = decoder
        .next()
        // Reader-local padding is not peer input and must not inflate errors.
        .map_err(|error| GhosttyEngineError::checkpoint(error, input.len()))?;
    require_consumed_unit(decoder, staged, input.len())?;
    Ok(progress)
}

fn require_consumed_unit(
    decoder: &FeedIncrementalDecoder<'_, '_>,
    offered: usize,
    wire_bytes: usize,
) -> Result<(), GhosttyEngineError> {
    let unread = decoder.staged_bytes();
    if unread != 0 {
        return Err(GhosttyEngineError::checkpoint(
            libghostty_vt::Error::InvalidValue,
            offered.saturating_sub(unread).min(wire_bytes),
        ));
    }
    Ok(())
}

fn fail_history_decoder(native: &mut NativeReplica) {
    let state = std::mem::replace(&mut native.decoder, NativeDecoderState::Failed);
    // Release both the staged allocation and failed native decoder, while
    // preserving the live terminal for rendering and live output. CRC/framing
    // rejection leaves it unchanged; engine semantic errors are not transactional.
    native.decoder = match state {
        NativeDecoderState::Ready(decoder) => {
            native.history_failed = true;
            NativeDecoderState::Finished(decoder.into_terminal())
        }
        other => other,
    };
}

fn finish_history_decoder(native: &mut NativeReplica) -> HistoryApplyOutcome {
    let state = std::mem::replace(&mut native.decoder, NativeDecoderState::Failed);
    let NativeDecoderState::Ready(decoder) = state else {
        unreachable!("FINISH was authenticated from the ready decoder")
    };
    native.decoder = NativeDecoderState::Finished(decoder.into_terminal());
    HistoryApplyOutcome {
        progress: BootstrapProgress::Finished,
        retained_rows: 0,
        authenticated_rows: 0,
    }
}
