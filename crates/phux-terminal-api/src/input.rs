//! Ordered engine input. Encoders are constructed on the lane that uses them;
//! they need not be Send. The host rechecks authority immediately before submit.

use crate::TerminalInput;
use crate::actor::{EncodedInputRequest, InputCreditPool};
use std::sync::Arc;

pub struct LaneAhead(Box<dyn Send>);
impl LaneAhead {
    pub fn new(guard: impl Send + 'static) -> Self {
        Self(Box::new(guard))
    }
}
impl std::fmt::Debug for LaneAhead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = &self.0;
        f.write_str("LaneAhead")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectWrite {
    Wrote,
    Handled,
    Busy,
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("unsafe paste")]
    UnsafePaste,
    #[error("encoded input exceeds its byte budget")]
    LimitExceeded,
    #[error("{0}")]
    Engine(String),
}

/// Fully encoded bytes prepared by the engine for exactly one generation.
#[derive(Debug)]
pub struct PreparedInput {
    bytes: Vec<u8>,
    echo_probe: bool,
}
impl PreparedInput {
    #[must_use]
    pub const fn new(bytes: Vec<u8>, echo_probe: bool) -> Self {
        Self { bytes, echo_probe }
    }
    #[must_use]
    pub fn into_request(self) -> EncodedInputRequest {
        EncodedInputRequest::legacy_probe(self.bytes, self.echo_probe)
    }
}

pub trait LaneEncoder: std::fmt::Debug {
    fn is_closed(&self) -> bool;
    fn prepare(&mut self, input: &TerminalInput) -> Result<Option<PreparedInput>, InputError>;
    /// One atomic mode snapshot for the whole batch; reject unsafe pastes before
    /// encoding anything and bound the aggregate encoded bytes.
    fn prepare_batch(
        &mut self,
        inputs: &[TerminalInput],
        max_bytes: usize,
    ) -> Result<PreparedInput, InputError>;
}

pub trait OrderedInput: std::fmt::Debug + Send + Sync {
    fn encoder(&self) -> Result<Box<dyn LaneEncoder>, InputError>;
    fn credits(&self) -> InputCreditPool;
    fn submit(
        &self,
        request: EncodedInputRequest,
    ) -> Result<(), tokio::sync::mpsc::error::TrySendError<EncodedInputRequest>>;
    fn arm_lane(&self) -> Option<LaneAhead>;
    fn try_inline(&self, input: &TerminalInput) -> DirectWrite;
}

#[derive(Debug, Clone)]
pub struct InputPort(Arc<dyn OrderedInput>);
impl InputPort {
    pub fn new(port: impl OrderedInput + 'static) -> Self {
        Self(Arc::new(port))
    }
    #[must_use]
    pub fn same_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn encoder(&self) -> Result<Box<dyn LaneEncoder>, InputError> {
        self.0.encoder()
    }
    #[must_use]
    pub fn credits(&self) -> InputCreditPool {
        self.0.credits()
    }
    pub fn submit(
        &self,
        request: EncodedInputRequest,
    ) -> Result<(), tokio::sync::mpsc::error::TrySendError<EncodedInputRequest>> {
        self.0.submit(request)
    }
    #[must_use]
    pub fn arm_lane(&self) -> Option<LaneAhead> {
        self.0.arm_lane()
    }
    #[must_use]
    pub fn try_inline(&self, input: &TerminalInput) -> DirectWrite {
        self.0.try_inline(input)
    }
}

/// A disconnected host-only fixture; it links no terminal implementation.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct DetachedInput(InputCreditPool);
#[cfg(any(test, feature = "test-support"))]
impl OrderedInput for DetachedInput {
    fn encoder(&self) -> Result<Box<dyn LaneEncoder>, InputError> {
        Err(InputError::Engine("detached input".into()))
    }
    fn credits(&self) -> InputCreditPool {
        self.0.clone()
    }
    fn submit(
        &self,
        request: EncodedInputRequest,
    ) -> Result<(), tokio::sync::mpsc::error::TrySendError<EncodedInputRequest>> {
        Err(tokio::sync::mpsc::error::TrySendError::Closed(request))
    }
    fn arm_lane(&self) -> Option<LaneAhead> {
        None
    }
    fn try_inline(&self, _: &TerminalInput) -> DirectWrite {
        DirectWrite::Busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::{INPUT_CREDITS, WriteCompletion, WriteCompletionSink};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct DropSignal(Arc<AtomicUsize>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn a_discarded_write_releases_credit_lane_guard_and_completion() {
        let port = InputPort::new(DetachedInput::default());
        let credits = port.credits();
        let dropped = Arc::new(AtomicUsize::new(0));
        let (sink, completed) = WriteCompletionSink::channel();
        let mut request = PreparedInput::new(vec![b'x'], true)
            .into_request()
            .with_credit(credits.try_take().expect("credit"))
            .with_lane_ahead(Some(LaneAhead::new(DropSignal(dropped.clone()))));
        request.completion = Some(sink);
        assert_eq!(credits.available(), INPUT_CREDITS - 1);
        let refused = port.submit(request).expect_err("disconnected writer");
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(refused);
        assert_eq!(credits.available(), INPUT_CREDITS);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(
            completed.recv().expect("drop receipt"),
            WriteCompletion::Failed
        );
    }

    #[test]
    fn clones_preserve_generation_but_a_new_port_never_inherits_it() {
        let first = InputPort::new(DetachedInput::default());
        assert!(first.same_generation(&first.clone()));
        let second = InputPort::new(DetachedInput::default());
        assert!(!first.same_generation(&second));
    }
}
