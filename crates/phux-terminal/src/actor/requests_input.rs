use crate::TerminalInput;
use bytes::Bytes;
use tokio::sync::mpsc;

/// Default depth of the per-pane input mailbox.
pub const DEFAULT_INPUT_MAILBOX: usize = 64;

/// Input credits per pane (ADR-0144): credited requests in flight between
/// their sender and the end of their `write(2)`.
///
/// Equal to the encoded-input mailbox depth, so a credited `try_send` never
/// finds the mailbox full.
pub const INPUT_CREDITS: usize = DEFAULT_INPUT_MAILBOX;

/// Writer-queue slots that only uncredited terminal replies may use.
const TERMINAL_REPLY_HEADROOM: usize = 16;

/// PTY writer queue depth: every input credit plus the reply headroom, so a
/// credited request forwarded by the actor always finds a slot.
pub const PTY_WRITER_QUEUE: usize = INPUT_CREDITS + TERMINAL_REPLY_HEADROOM;

/// One pane's input credits (ADR-0144). Cloning shares the pool.
#[derive(Debug, Clone)]
pub struct InputCreditPool(std::sync::Arc<tokio::sync::Semaphore>);

impl Default for InputCreditPool {
    fn default() -> Self {
        Self(std::sync::Arc::new(tokio::sync::Semaphore::new(
            INPUT_CREDITS,
        )))
    }
}

impl InputCreditPool {
    /// A credit, if one is free now.
    #[must_use]
    pub fn try_take(&self) -> Option<InputCredit> {
        self.0.try_acquire().ok()?.forget();
        Some(InputCredit { pool: self.clone() })
    }

    /// A credit, waiting until one is returned.
    pub async fn take(&self) -> InputCredit {
        // The pool is never closed, so `acquire` only fails if it were.
        if let Ok(permit) = self.0.acquire().await {
            permit.forget();
        }
        InputCredit { pool: self.clone() }
    }

    /// Credits free right now.
    #[must_use]
    pub fn available(&self) -> usize {
        self.0.available_permits()
    }

    fn is(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One held pane input credit, returned to its pool when dropped.
#[derive(Debug)]
pub struct InputCredit {
    pool: InputCreditPool,
}

impl InputCredit {
    /// Whether this credit was drawn from `pool`.
    #[must_use]
    pub fn is_from(&self, pool: &InputCreditPool) -> bool {
        self.pool.is(pool)
    }
}

impl Drop for InputCredit {
    fn drop(&mut self) {
        self.pool.0.add_permits(1);
    }
}

/// Queue `request` for the PTY writer.
///
/// An uncredited request (a terminal
/// reply, or input from the no-lane path) may not take a slot reserved for
/// credited input, so it is refused as `Full` once only those remain.
///
/// # Errors
///
/// The request back, as `try_send` would return it.
pub fn try_send_to_writer(
    tx: &mpsc::Sender<EncodedInputRequest>,
    mut request: EncodedInputRequest,
) -> Result<(), mpsc::error::TrySendError<EncodedInputRequest>> {
    let result = if request.credit.is_none() && tx.capacity() <= INPUT_CREDITS {
        Err(mpsc::error::TrySendError::Full(request))
    } else {
        request.writer_queued_at = Some(std::time::Instant::now());
        tx.try_send(request)
    };
    if matches!(&result, Err(mpsc::error::TrySendError::Full(_))) {
        phux_resource::perf::INPUT_WRITER_FULL.incr();
    }
    result
}

/// Final disposition of a PTY write, reported on
/// [`EncodedInputRequest::completion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteCompletion {
    /// `write_all` and `flush` both succeeded.
    Delivered,
    /// Failed after bytes may have landed; delivery is indeterminate.
    Failed,
    /// Refused before writing: the canonical-mode line discipline would have
    /// truncated a line longer than `limit` and possibly wedged the pane.
    CanonicalLimitExceeded { limit: usize },
    /// Never reached a live writer, so `write(2)` was provably not called
    /// (no PTY, full or closed queue). Safe to resubmit.
    NotWritten,
}

/// One-shot report path for an acknowledged write's [`WriteCompletion`],
/// tagging the outcome with its admission-queue ticket.
///
/// Dropping an unfired sink reports [`WriteCompletion::Failed`]; sites that
/// can prove nothing was written report [`WriteCompletion::NotWritten`].
pub struct WriteCompletionSink {
    notify: Option<Box<dyn FnOnce(WriteCompletion) + Send>>,
}

impl WriteCompletionSink {
    pub fn new(notify: impl FnOnce(WriteCompletion) + Send + 'static) -> Self {
        Self {
            notify: Some(Box::new(notify)),
        }
    }

    /// Report `outcome` once; consuming the sink disarms the `Drop` fallback.
    pub fn complete(mut self, outcome: WriteCompletion) {
        if let Some(notify) = self.notify.take() {
            notify(outcome);
        }
    }

    /// A sink backed by a plain channel (tests).
    #[cfg(test)]
    #[must_use]
    pub fn channel() -> (Self, std::sync::mpsc::Receiver<WriteCompletion>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Self::new(move |outcome| {
                let _ = tx.send(outcome);
            }),
            rx,
        )
    }
}

impl Drop for WriteCompletionSink {
    fn drop(&mut self) {
        if let Some(notify) = self.notify.take() {
            notify(WriteCompletion::Failed);
        }
    }
}

impl std::fmt::Debug for WriteCompletionSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteCompletionSink")
            .field("armed", &self.notify.is_some())
            .finish()
    }
}

/// Bytes for the PTY writer, optionally with a completion report.
#[derive(Debug)]
pub struct EncodedInputRequest {
    /// Fully encoded PTY bytes.
    pub bytes: Bytes,
    /// Whether this may arm the `echo.server` sample (keys and pastes, not
    /// mouse, focus, or terminal replies).
    pub(crate) echo_probe: bool,
    /// See [`WriteCompletionSink`]; dropping it reports indeterminate.
    pub completion: Option<WriteCompletionSink>,
    /// The pane input credit this request holds (ADR-0144), released when
    /// the request drops: after the writer thread's `write(2)`, or at
    /// whichever step discards it.
    pub(crate) credit: Option<InputCredit>,
    /// Stamped only at the final writer handoff, not the lane/actor queue.
    pub(crate) writer_queued_at: Option<std::time::Instant>,
    /// Set by [`super::super::fast_pty::PtyFastPath::enqueue`]. The writer decrements
    /// the pane's pending count only for these, so a test `try_send` does not
    /// underflow it.
    pub(crate) counts_pending: bool,
    /// In-flight input-lane key. [`super::super::fast_pty::PtyFastPath::enqueue`]
    /// drops it after `pending` is published; every earlier discard drops it
    /// with the request.
    pub(crate) lane_ahead: Option<super::super::fast_pty::LaneAhead>,
}

impl EncodedInputRequest {
    #[must_use]
    pub fn legacy(bytes: Vec<u8>) -> Self {
        Self::legacy_probe(bytes, true)
    }

    /// [`Self::legacy`] with an explicit echo-probe flag.
    #[must_use]
    pub fn legacy_probe(bytes: Vec<u8>, echo_probe: bool) -> Self {
        Self {
            bytes: bytes.into(),
            echo_probe,
            completion: None,
            credit: None,
            writer_queued_at: None,
            counts_pending: false,
            lane_ahead: None,
        }
    }

    /// This request, holding `credit` until it is written or discarded.
    #[must_use]
    pub fn with_credit(mut self, credit: InputCredit) -> Self {
        self.credit = Some(credit);
        self
    }

    /// This request keeps the inline path closed until the writer queue
    /// counts it.
    #[must_use]
    pub fn with_lane_ahead(
        mut self,
        lane_ahead: Option<super::super::fast_pty::LaneAhead>,
    ) -> Self {
        self.lane_ahead = lane_ahead;
        self
    }

    #[must_use]
    pub fn acknowledged(bytes: Vec<u8>, completion: WriteCompletionSink) -> Self {
        Self {
            bytes: Bytes::from(bytes),
            echo_probe: true,
            completion: Some(completion),
            credit: None,
            writer_queued_at: None,
            counts_pending: false,
            lane_ahead: None,
        }
    }
}

/// Whether this input kind arms the `echo.server` sample.
#[must_use]
pub const fn echo_probe_for(input: &TerminalInput) -> bool {
    matches!(input, TerminalInput::Key(_) | TerminalInput::Paste(_))
}
