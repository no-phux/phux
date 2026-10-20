//! Typed checkout methods: correlated Git reads and index writes on the
//! checkout a Terminal is in, with a lossless answer queue.

use phux_protocol::checkout::{
    CheckoutAnswer, CheckoutArea, CheckoutRequest, MAX_CHECKOUT_REQUEST_BYTES, encoded_request_len,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};

use super::{ControlPlane, Pending, ServerFeatureExt};

/// Maximum outstanding plus undrained checkout answers on one client. Drain
/// answers before issuing more; zero from `checkout` means nothing was sent.
pub const MAX_CHECKOUT_ANSWERS: usize = 64;

/// Why a checkout request has no server answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutFailure {
    /// The connection's grant lacks `INPUT` on the Terminal.
    PermissionDenied,
    /// No such Terminal.
    TerminalNotFound,
    /// The hub's satellite does not serve checkout methods.
    Unsupported,
    /// The server is saturated; retry.
    Busy,
    /// The connection ended before the answer.
    Unanswered,
    /// Another refusal; see the message.
    Other,
}

/// How one checkout request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutOutcome {
    /// The server answered, possibly with a typed refusal of its own.
    Answered(Box<CheckoutAnswer>),
    /// The command was refused before the checkout was read, or never
    /// answered.
    Failed {
        /// The class of failure.
        failure: CheckoutFailure,
        /// Display-only detail.
        message: String,
    },
}

/// One correlated checkout answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutResponse {
    /// The correlation `checkout` returned.
    pub request_id: u32,
    /// The Terminal the request named.
    pub terminal_id: ResourceId,
    /// What happened.
    pub outcome: CheckoutOutcome,
}

impl ControlPlane {
    /// Ask the server that owns `terminal_id` for one checkout operation.
    /// Returns zero, sending nothing, when disconnected, when the server
    /// predates `CHECKOUT`, when the request is malformed or too large, or
    /// when the bounded answer queue is full.
    pub fn checkout(&mut self, terminal_id: ResourceId, request: CheckoutRequest) -> u32 {
        if !self.checkout_ready() || !sendable(&request) {
            return 0;
        }
        let request_id = self.next_request_id();
        self.pending
            .insert(request_id, Pending::Checkout(terminal_id.clone()));
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::Checkout {
                terminal_id,
                request,
            },
        });
        request_id
    }

    /// Drain checkout answers. No answer is dropped to make room.
    pub fn take_checkout_answers(&mut self) -> Vec<CheckoutResponse> {
        std::mem::take(&mut self.extensions.checkout_answers)
    }

    fn checkout_ready(&self) -> bool {
        let in_flight = self
            .pending
            .values()
            .filter(|pending| matches!(pending, Pending::Checkout(_)))
            .count();
        self.handshake_ready
            && self
                .server
                .as_ref()
                .is_some_and(|server| server.has_ext(ServerFeatureExt::Checkout))
            && in_flight + self.extensions.checkout_answers.len() < MAX_CHECKOUT_ANSWERS
    }

    pub(super) fn resolve_checkout(
        &mut self,
        request_id: u32,
        terminal_id: ResourceId,
        result: CommandResult,
    ) {
        let outcome = match result {
            CommandResult::OkWith(CommandValue::Checkout(answer)) => {
                CheckoutOutcome::Answered(answer)
            }
            CommandResult::Error { code, message } => CheckoutOutcome::Failed {
                failure: failure(code),
                message,
            },
            other => CheckoutOutcome::Failed {
                failure: CheckoutFailure::Other,
                message: format!("unexpected CHECKOUT reply: {other:?}"),
            },
        };
        self.extensions.checkout_answers.push(CheckoutResponse {
            request_id,
            terminal_id,
            outcome,
        });
    }

    /// Answer every outstanding checkout as unanswered.
    pub(super) fn reset_checkouts(&mut self, message: &str) {
        let mut outstanding: Vec<(u32, ResourceId)> = self
            .pending
            .iter()
            .filter_map(|(request_id, pending)| match pending {
                Pending::Checkout(terminal_id) => Some((*request_id, terminal_id.clone())),
                _ => None,
            })
            .collect();
        outstanding.sort_by_key(|(request_id, _)| *request_id);
        for (request_id, terminal_id) in outstanding {
            self.pending.remove(&request_id);
            self.extensions.checkout_answers.push(CheckoutResponse {
                request_id,
                terminal_id,
                outcome: CheckoutOutcome::Failed {
                    failure: CheckoutFailure::Unanswered,
                    message: message.to_owned(),
                },
            });
        }
    }
}

fn sendable(request: &CheckoutRequest) -> bool {
    let known_area = !matches!(
        request,
        CheckoutRequest::Diff {
            area: CheckoutArea::Unknown,
            ..
        }
    );
    known_area && encoded_request_len(request) <= MAX_CHECKOUT_REQUEST_BYTES
}

const fn failure(code: ErrorCode) -> CheckoutFailure {
    match code {
        ErrorCode::PermissionDenied => CheckoutFailure::PermissionDenied,
        ErrorCode::TerminalNotFound => CheckoutFailure::TerminalNotFound,
        ErrorCode::UnsupportedSatelliteRoute => CheckoutFailure::Unsupported,
        ErrorCode::ResourceExhausted => CheckoutFailure::Busy,
        _ => CheckoutFailure::Other,
    }
}
