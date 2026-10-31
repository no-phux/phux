//! Per-pane encoder sets and the handoff of encoded bytes to a pane actor.

use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use tokio::sync::mpsc;

use phux_terminal::TerminalInput;
use phux_terminal::actor::{EncodedInputRequest, InputCredit, LaneAhead, TerminalHandle};
use phux_terminal::input::{
    InputEncoderSnapshot, PasteOutcome, PerTerminalFocusEncoder, PerTerminalKeyEncoder,
    PerTerminalMouseEncoder, PerTerminalPasteEncoder,
};

use super::credit;

/// One pane generation's encoders, fed by its actor's published snapshot.
#[derive(Debug)]
pub(super) struct LaneEncoderSet {
    key: PerTerminalKeyEncoder,
    mouse: PerTerminalMouseEncoder,
    focus: PerTerminalFocusEncoder,
    paste: PerTerminalPasteEncoder,
    pub(super) snapshot: tokio::sync::watch::Receiver<InputEncoderSnapshot>,
}

impl LaneEncoderSet {
    fn new(handle: &TerminalHandle) -> Result<Self, phux_terminal::VtError> {
        Ok(Self {
            key: PerTerminalKeyEncoder::new()?,
            mouse: PerTerminalMouseEncoder::new()?,
            focus: PerTerminalFocusEncoder::new(),
            paste: PerTerminalPasteEncoder::new(),
            snapshot: handle.input_snapshot.clone(),
        })
    }

    fn encode(&mut self, input: &TerminalInput) -> Result<Option<Vec<u8>>, phux_terminal::VtError> {
        let snapshot = *self.snapshot.borrow();
        self.encode_with_snapshot(input, snapshot)
    }

    pub(super) fn encode_with_snapshot(
        &mut self,
        input: &TerminalInput,
        snapshot: InputEncoderSnapshot,
    ) -> Result<Option<Vec<u8>>, phux_terminal::VtError> {
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

pub(super) fn prune_closed_encoders(
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
) {
    encoders.retain(|_, encoder| encoder.snapshot.has_changed().is_ok());
}

pub(super) fn encoder_for<'a>(
    encoders: &'a mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    pane: phux_core::ids::ResourceId,
    handle: &TerminalHandle,
) -> Result<&'a mut LaneEncoderSet, phux_terminal::VtError> {
    let replace = encoders
        .get(&pane)
        .is_some_and(|encoder| !encoder.snapshot.same_channel(&handle.input_snapshot));
    if replace {
        encoders.remove(&pane);
    }
    match encoders.entry(pane) {
        std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
        std::collections::hash_map::Entry::Vacant(entry) => {
            Ok(entry.insert(LaneEncoderSet::new(handle)?))
        }
    }
}

pub(super) fn encode_input(
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    pane: phux_core::ids::ResourceId,
    handle: &TerminalHandle,
    input: &TerminalInput,
) -> Option<Vec<u8>> {
    match encoder_for(encoders, pane, handle).and_then(|encoder| encoder.encode(input)) {
        Ok(Some(bytes)) if !bytes.is_empty() => Some(bytes),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(error = %err, ?pane, "input lane encode failed; dropping event");
            None
        }
    }
}

/// The refusal for input that found its pane saturated at the handoff.
fn input_saturated() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::ResourceExhausted,
        message: "terminal input is saturated; nothing was written".to_owned(),
    }
}

pub(super) fn handoff_encoded(
    pane: phux_core::ids::ResourceId,
    handle: &TerminalHandle,
    bytes: Option<Vec<u8>>,
    echo_probe: bool,
    credit: Option<InputCredit>,
    lane_ahead: Option<LaneAhead>,
) -> Result<bool, CommandResult> {
    let Some(bytes) = bytes else {
        drop(lane_ahead);
        return Ok(true);
    };
    // A sender that waited brings its credit; only input that raced a pane
    // replacement, or came from a sender that did not wait, can find none.
    let Some(credit) = credit::credit_for(handle, credit) else {
        drop(lane_ahead);
        tracing::warn!(?pane, "pane input credits exhausted; refusing input");
        return Err(input_saturated());
    };
    let request = EncodedInputRequest::legacy_probe(bytes, echo_probe)
        .with_credit(credit)
        .with_lane_ahead(lane_ahead);
    match handle.encoded_input.try_send(request) {
        Ok(()) => Ok(true),
        // Unreachable while every sender holds a credit (ADR-0144).
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::error!(
                ?pane,
                "credited input found the pane mailbox full; refusing"
            );
            Err(input_saturated())
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err(CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for ROUTE_INPUT".to_owned(),
        }),
    }
}
