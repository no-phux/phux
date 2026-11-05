//! Concrete lane-local encoding and ordered input port.

use phux_terminal_api::input::{
    DirectWrite, InputError, LaneAhead, LaneEncoder, OrderedInput, PreparedInput,
};
use tokio::sync::mpsc;

use crate::TerminalInput;
use crate::actor::{EncodedInputRequest, InputCreditPool};
use crate::input::{
    InputEncoderSnapshot, PasteOutcome, PerTerminalFocusEncoder, PerTerminalKeyEncoder,
    PerTerminalMouseEncoder, PerTerminalPasteEncoder,
};

#[derive(Debug)]
pub(crate) struct EngineInput {
    pub(crate) encoded: mpsc::Sender<EncodedInputRequest>,
    pub(crate) credits: InputCreditPool,
    pub(crate) snapshot: tokio::sync::watch::Receiver<InputEncoderSnapshot>,
    pub(crate) fast: Option<std::sync::Weak<crate::actor::fast_pty::PtyFastPath>>,
}

impl OrderedInput for EngineInput {
    fn encoder(&self) -> Result<Box<dyn LaneEncoder>, InputError> {
        Ok(Box::new(
            LaneEncoderSet::new(self.snapshot.clone()).map_err(input_error)?,
        ))
    }
    fn credits(&self) -> InputCreditPool {
        self.credits.clone()
    }
    fn submit(
        &self,
        request: EncodedInputRequest,
    ) -> Result<(), mpsc::error::TrySendError<EncodedInputRequest>> {
        self.encoded.try_send(request)
    }
    fn arm_lane(&self) -> Option<LaneAhead> {
        let fast = self.fast.as_ref()?.upgrade()?;
        Some(LaneAhead::new(crate::actor::fast_pty::LaneAhead::arm(
            &fast,
        )))
    }
    fn try_inline(&self, input: &TerminalInput) -> DirectWrite {
        let TerminalInput::Key(event) = input else {
            return DirectWrite::Busy;
        };
        let options = self.snapshot.borrow().key;
        let Some(text) = crate::input::key::legacy_printable(event, options) else {
            return DirectWrite::Busy;
        };
        let Some(fast) = self.fast.as_ref().and_then(std::sync::Weak::upgrade) else {
            return DirectWrite::Busy;
        };
        fast.try_direct(text.as_bytes())
    }
}

fn input_error(error: crate::VtError) -> InputError {
    InputError::Engine(error.to_string())
}

/// One pane generation's encoders, fed by its actor's published snapshot.
#[derive(Debug)]
struct LaneEncoderSet {
    key: PerTerminalKeyEncoder,
    mouse: PerTerminalMouseEncoder,
    focus: PerTerminalFocusEncoder,
    paste: PerTerminalPasteEncoder,
    snapshot: tokio::sync::watch::Receiver<InputEncoderSnapshot>,
}

impl LaneEncoderSet {
    fn new(
        snapshot: tokio::sync::watch::Receiver<InputEncoderSnapshot>,
    ) -> Result<Self, crate::VtError> {
        Ok(Self {
            key: PerTerminalKeyEncoder::new()?,
            mouse: PerTerminalMouseEncoder::new()?,
            focus: PerTerminalFocusEncoder::new(),
            paste: PerTerminalPasteEncoder::new(),
            snapshot,
        })
    }

    fn encode(&mut self, input: &TerminalInput) -> Result<Option<Vec<u8>>, crate::VtError> {
        let snapshot = *self.snapshot.borrow();
        self.encode_with_snapshot(input, snapshot)
    }

    pub(super) fn encode_with_snapshot(
        &mut self,
        input: &TerminalInput,
        snapshot: InputEncoderSnapshot,
    ) -> Result<Option<Vec<u8>>, crate::VtError> {
        match input {
            TerminalInput::Key(event) => Ok(Some(
                self.key.encode_with_options(event, snapshot.key)?.to_vec(),
            )),
            TerminalInput::Mouse(event) => Ok(Some(
                self.mouse
                    .encode_with_options(
                        event,
                        snapshot.mouse,
                        snapshot.cols,
                        snapshot.rows,
                        snapshot.cell_px,
                    )?
                    .to_vec(),
            )),
            TerminalInput::Focus(event) => Ok(self
                .focus
                .encode_with_mode(*event, snapshot.focus_reporting)?
                .map(<[u8]>::to_vec)),
            TerminalInput::Paste(event) => match self
                .paste
                .encode_with_mode(event, snapshot.bracketed_paste)?
            {
                PasteOutcome::Encoded(bytes) => Ok(Some(bytes.to_vec())),
                PasteOutcome::Rejected => Ok(None),
            },
        }
    }
}

impl LaneEncoder for LaneEncoderSet {
    fn is_closed(&self) -> bool {
        self.snapshot.has_changed().is_err()
    }
    fn prepare(&mut self, input: &TerminalInput) -> Result<Option<PreparedInput>, InputError> {
        Ok(self
            .encode(input)
            .map_err(input_error)?
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| PreparedInput::new(bytes, crate::actor::echo_probe_for(input))))
    }
    fn prepare_batch(
        &mut self,
        inputs: &[TerminalInput],
        max_bytes: usize,
    ) -> Result<PreparedInput, InputError> {
        if inputs.iter().any(|input| matches!(input, TerminalInput::Paste(event) if PerTerminalPasteEncoder::would_reject(event))) {
            return Err(InputError::UnsafePaste);
        }
        let snapshot = *self.snapshot.borrow();
        let mut bytes = Vec::new();
        for input in inputs {
            if let Some(event_bytes) = self
                .encode_with_snapshot(input, snapshot)
                .map_err(input_error)?
            {
                if bytes.len().saturating_add(event_bytes.len()) > max_bytes {
                    return Err(InputError::LimitExceeded);
                }
                bytes.extend_from_slice(&event_bytes);
            }
        }
        Ok(PreparedInput::new(bytes, true))
    }
}
