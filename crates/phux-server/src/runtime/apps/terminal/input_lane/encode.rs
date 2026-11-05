//! Host generation cache and authorized handoff; encoding belongs to the engine.
use super::credit;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use phux_terminal_api::TerminalInput;
use phux_terminal_api::actor::{InputCredit, LaneAhead, TerminalHandle};
use phux_terminal_api::input::{InputError, InputPort, LaneEncoder, PreparedInput};

#[derive(Debug)]
pub(super) struct LaneEncoderSet {
    pub(super) port: InputPort,
    encoder: Box<dyn LaneEncoder>,
}
impl LaneEncoderSet {
    pub(super) fn prepare_batch(
        &mut self,
        inputs: &[TerminalInput],
        max_bytes: usize,
    ) -> Result<PreparedInput, InputError> {
        self.encoder.prepare_batch(inputs, max_bytes)
    }
}
pub(super) fn prune_closed_encoders(
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
) {
    encoders.retain(|_, encoder| !encoder.encoder.is_closed());
}
pub(super) fn encoder_for<'a>(
    encoders: &'a mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    pane: phux_core::ids::ResourceId,
    handle: &TerminalHandle,
) -> Result<&'a mut LaneEncoderSet, InputError> {
    if encoders
        .get(&pane)
        .is_some_and(|encoder| !encoder.port.same_generation(&handle.ordered_input))
    {
        encoders.remove(&pane);
    }
    match encoders.entry(pane) {
        std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
        std::collections::hash_map::Entry::Vacant(entry) => Ok(entry.insert(LaneEncoderSet {
            port: handle.ordered_input.clone(),
            encoder: handle.ordered_input.encoder()?,
        })),
    }
}
pub(super) fn encode_input(
    encoders: &mut std::collections::HashMap<phux_core::ids::ResourceId, LaneEncoderSet>,
    pane: phux_core::ids::ResourceId,
    handle: &TerminalHandle,
    input: &TerminalInput,
) -> Option<PreparedInput> {
    match encoder_for(encoders, pane, handle).and_then(|encoder| encoder.encoder.prepare(input)) {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::warn!(error = %err, ?pane, "input lane encode failed; dropping event");
            None
        }
    }
}
fn input_saturated() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::ResourceExhausted,
        message: "terminal input is saturated; nothing was written".into(),
    }
}
pub(super) fn handoff_encoded(
    pane: phux_core::ids::ResourceId,
    handle: &TerminalHandle,
    prepared: Option<PreparedInput>,
    credit: Option<InputCredit>,
    lane_ahead: Option<LaneAhead>,
) -> Result<bool, CommandResult> {
    let Some(prepared) = prepared else {
        drop(lane_ahead);
        return Ok(true);
    };
    let Some(credit) = credit::credit_for(handle, credit) else {
        drop(lane_ahead);
        tracing::warn!(?pane, "pane input credits exhausted; refusing input");
        return Err(input_saturated());
    };
    let request = prepared
        .into_request()
        .with_credit(credit)
        .with_lane_ahead(lane_ahead);
    match handle.ordered_input.submit(request) {
        Ok(()) => Ok(true),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            tracing::error!(
                ?pane,
                "credited input found the pane mailbox full; refusing"
            );
            Err(input_saturated())
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for ROUTE_INPUT".into(),
        }),
    }
}
