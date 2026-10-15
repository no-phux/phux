//! Dedicated SDK commands and honest, non-PTY receipts.

use phux_protocol::native_agent::{
    MAX_NATIVE_PENDING, NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent,
    NativeRequest, validate_payload,
};

use super::{
    Command, CommandResult, CommandValue, ControlPlane, ErrorCode, Event, FrameKind, Pending,
    ResourceId,
};

impl ControlPlane {
    /// Submit an intent to this exact SDK-owned session and activation.
    ///
    /// Invalid intents, unavailable connections, and a full pending queue return
    /// `None` without scheduling anything. The returned operation id identifies
    /// a dedicated [`Event::NativeAgentResult`], not terminal input delivery.
    /// Connection loss resolves it as Unknown; model input is never replayed.
    #[must_use]
    pub fn native_agent_command(
        &mut self,
        agent_session_id: &ResourceId,
        activation: &str,
        intent: NativeIntent,
    ) -> Option<u64> {
        if !self.handshake_ready
            || activation.is_empty()
            || intent.validate().is_err()
            || self
                .pending
                .values()
                .filter(|pending| matches!(pending, Pending::NativeAgent { .. }))
                .count()
                >= MAX_NATIVE_PENDING
        {
            return None;
        }
        // Viewers do not share a counter. Random nonzero identities prevent a
        // desktop and a phone from accidentally reserving the same operation.
        let operation_id = loop {
            let bytes = *uuid::Uuid::new_v4().as_bytes();
            let id = u64::from_be_bytes(bytes[..8].try_into().ok()?);
            if id != 0
                && !self.pending.values().any(|pending| {
                    matches!(pending,
                Pending::NativeAgent { operation_id, .. } if *operation_id == id)
                })
            {
                break id;
            }
        };
        let request = NativeRequest::Command {
            agent_session_id: agent_session_id.clone(),
            command: NativeCommand {
                operation_id,
                activation: activation.to_owned(),
                intent,
            },
        };
        // The transport bounds the entire request, not just the inner intent.
        validate_payload(&request).ok()?;
        let request_id = self.next_request_id();
        self.pending.insert(
            request_id,
            Pending::NativeAgent {
                agent_session_id: agent_session_id.clone(),
                operation_id,
            },
        );
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::NativeAgent {
                request: Box::new(request),
            },
        });
        Some(operation_id)
    }

    pub(super) fn fail_native_pending(&mut self, message: &str) {
        // The admission bound lets us retire these correlations without an
        // allocation or disturbing reconnect-safe non-native operations.
        let mut native_ids = self.pending.iter().filter_map(|(&request_id, pending)| {
            matches!(pending, Pending::NativeAgent { .. }).then_some(request_id)
        });
        let request_ids: [Option<u32>; MAX_NATIVE_PENDING] =
            std::array::from_fn(|_| native_ids.next());
        for request_id in request_ids.into_iter().flatten() {
            if let Some(Pending::NativeAgent {
                agent_session_id,
                operation_id,
            }) = self.pending.remove(&request_id)
            {
                self.resolve_native_agent(
                    agent_session_id,
                    operation_id,
                    CommandResult::Error {
                        code: ErrorCode::InputDeliveryUnknown,
                        message: message.to_owned(),
                    },
                );
            }
        }
    }

    pub(super) fn resolve_native_agent(
        &mut self,
        agent_session_id: ResourceId,
        operation_id: u64,
        result: CommandResult,
    ) {
        let receipt = match result {
            CommandResult::OkWith(CommandValue::Json(json)) => {
                match serde_json::from_str::<NativeCommandResult>(&json) {
                    Ok(receipt) if receipt.operation_id == operation_id => receipt,
                    _ => NativeCommandResult {
                        operation_id,
                        disposition: NativeDisposition::Unknown,
                        detail: Some(
                            "invalid or mismatched native receipt; do not replay the intent".into(),
                        ),
                    },
                }
            }
            CommandResult::Error { code, message } => NativeCommandResult {
                operation_id,
                disposition: if matches!(
                    code,
                    ErrorCode::InputDeliveryUnknown
                        | ErrorCode::IncarnationChanged
                        | ErrorCode::InternalError
                ) {
                    NativeDisposition::Unknown
                } else {
                    NativeDisposition::Refused
                },
                detail: Some(message),
            },
            _ => NativeCommandResult {
                operation_id,
                disposition: NativeDisposition::Unknown,
                detail: Some(
                    "server did not return a native receipt; do not replay the intent".into(),
                ),
            },
        };
        self.push_event(Event::NativeAgentResult {
            agent_session_id,
            operation_id,
            disposition: receipt.disposition,
            detail: receipt.detail,
        });
    }
}
