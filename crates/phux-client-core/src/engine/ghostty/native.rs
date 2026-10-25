//! Native snapshot and history decoding, plus live effect hooks.

use super::{
    BootstrapProgress, Cell, ClipboardWrite, GhosttyEngineError, GhosttyTerminal,
    HistoryApplyOutcome, NativeDecoderState, NativeReplica, Rc, RefCell,
};

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
    decoder
        .feed(input)
        .map_err(|error| GhosttyEngineError::checkpoint(error, 0))?;
    Ok(BootstrapProgress::Pending)
}

fn attach_native_callbacks(
    native: &NativeReplica,
    terminal: &mut GhosttyTerminal<'static, 'static>,
) -> Result<(), GhosttyEngineError> {
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(terminal)?;
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
}

impl PendingEffects {
    pub(super) fn clear(&self) {
        self.bell.set(false);
        self.clipboard.take();
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
        .find(|content| content.mime.starts_with("text/plain"))?;
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
    let NativeDecoderState::Ready(decoder) = &mut native.decoder else {
        return Err(match native.decoder {
            NativeDecoderState::Finished(_) => GhosttyEngineError::InputAfterFinish,
            NativeDecoderState::Failed => GhosttyEngineError::DecoderFailed,
            NativeDecoderState::Collecting(_) | NativeDecoderState::Ready(_) => {
                GhosttyEngineError::HistoryBeforePublication
            }
        });
    };
    decoder
        .feed(input)
        .map_err(|error| GhosttyEngineError::checkpoint(error, 0))?;
    let staged = decoder.staged_bytes();
    let expected_rows =
        usize::try_from(declared_rows).map_err(|_| GhosttyEngineError::DecoderFailed)?;
    let Some(progress) = decoder
        .next_checked(expected_rows)
        .map_err(|error| GhosttyEngineError::checkpoint(error, staged))?
    else {
        return Ok(finish_history_decoder(native));
    };
    Ok(HistoryApplyOutcome {
        progress: BootstrapProgress::Ready,
        retained_rows: progress.rows,
        authenticated_rows: progress.authenticated_rows,
    })
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
