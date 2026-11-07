//! Terminal operations are owned by the pure API crate.
pub use phux_terminal_api::actor::*;

/// Native writer queue admission, including reserved reply headroom.
pub fn try_send_to_writer(
    tx: &tokio::sync::mpsc::Sender<EncodedInputRequest>,
    mut request: EncodedInputRequest,
) -> Result<(), tokio::sync::mpsc::error::TrySendError<EncodedInputRequest>> {
    let result = if request.credit.is_none() && tx.capacity() <= INPUT_CREDITS {
        Err(tokio::sync::mpsc::error::TrySendError::Full(request))
    } else {
        request.writer_queued_at = Some(std::time::Instant::now());
        tx.try_send(request)
    };
    if matches!(
        &result,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_))
    ) {
        crate::perf::INPUT_WRITER_FULL.incr();
    }
    result
}
