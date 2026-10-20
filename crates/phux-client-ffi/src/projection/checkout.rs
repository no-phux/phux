//! Checkout answers, as a product reads them: one typed answer or one
//! failure class, whether the server refused, the command never reached the
//! checkout, or the connection ended first.

use phux_client_runtime::control::{CheckoutFailure, CheckoutOutcome, CheckoutResponse};
use phux_protocol::checkout::{CheckoutAnswer, CheckoutRefusalCode};

use super::id;

/// Why a checkout request produced no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutError {
    CwdUnknown,
    NotARepository,
    InvalidPath,
    NotFound,
    InvalidRequest,
    Timeout,
    Busy,
    GitFailed,
    ReadFailed,
    GhUnavailable,
    GhUnauthenticated,
    NoPullRequest,
    GhFailed,
    PermissionDenied,
    TerminalNotFound,
    Unsupported,
    Unanswered,
    Other,
}

/// One checkout result: an answer that is never a refusal, or a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutReply {
    /// A successful answer; never [`CheckoutAnswer::Refused`].
    Answer(Box<CheckoutAnswer>),
    /// No answer, and why. The message is display-only.
    Failed {
        error: CheckoutError,
        message: String,
    },
}

/// One correlated checkout result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutResult {
    /// The correlation `checkout_query` returned.
    pub request_id: u32,
    /// The Terminal the request named, encoded like every other id.
    pub terminal_id: String,
    /// What happened.
    pub reply: CheckoutReply,
}

/// Project one runtime checkout response for every binding.
#[must_use]
pub fn result(response: CheckoutResponse) -> CheckoutResult {
    let reply = match response.outcome {
        CheckoutOutcome::Answered(answer) => match *answer {
            CheckoutAnswer::Refused(refusal) => CheckoutReply::Failed {
                error: refusal_error(refusal.code),
                message: refusal.message,
            },
            answer => CheckoutReply::Answer(Box::new(answer)),
        },
        CheckoutOutcome::Failed { failure, message } => CheckoutReply::Failed {
            error: failure_error(failure),
            message,
        },
    };
    CheckoutResult {
        request_id: response.request_id,
        terminal_id: id::encode(&response.terminal_id),
        reply,
    }
}

const fn refusal_error(code: CheckoutRefusalCode) -> CheckoutError {
    match code {
        CheckoutRefusalCode::CwdUnknown => CheckoutError::CwdUnknown,
        CheckoutRefusalCode::NotARepository => CheckoutError::NotARepository,
        CheckoutRefusalCode::InvalidPath => CheckoutError::InvalidPath,
        CheckoutRefusalCode::NotFound => CheckoutError::NotFound,
        CheckoutRefusalCode::InvalidRequest => CheckoutError::InvalidRequest,
        CheckoutRefusalCode::Timeout => CheckoutError::Timeout,
        CheckoutRefusalCode::Busy => CheckoutError::Busy,
        CheckoutRefusalCode::GitFailed => CheckoutError::GitFailed,
        CheckoutRefusalCode::ReadFailed => CheckoutError::ReadFailed,
        CheckoutRefusalCode::GhUnavailable => CheckoutError::GhUnavailable,
        CheckoutRefusalCode::GhUnauthenticated => CheckoutError::GhUnauthenticated,
        CheckoutRefusalCode::NoPullRequest => CheckoutError::NoPullRequest,
        CheckoutRefusalCode::GhFailed => CheckoutError::GhFailed,
        CheckoutRefusalCode::Unknown => CheckoutError::Other,
    }
}

const fn failure_error(failure: CheckoutFailure) -> CheckoutError {
    match failure {
        CheckoutFailure::PermissionDenied => CheckoutError::PermissionDenied,
        CheckoutFailure::TerminalNotFound => CheckoutError::TerminalNotFound,
        CheckoutFailure::Unsupported => CheckoutError::Unsupported,
        CheckoutFailure::Busy => CheckoutError::Busy,
        CheckoutFailure::Unanswered => CheckoutError::Unanswered,
        CheckoutFailure::Other => CheckoutError::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ResourceId;
    use phux_protocol::checkout::{CheckoutLog, CheckoutRefusal};

    fn response(outcome: CheckoutOutcome) -> CheckoutResponse {
        CheckoutResponse {
            request_id: 7,
            terminal_id: ResourceId::local(3),
            outcome,
        }
    }

    #[test]
    fn server_refusals_and_runtime_failures_share_one_error_vocabulary() {
        let refused = result(response(CheckoutOutcome::Answered(Box::new(
            CheckoutAnswer::Refused(CheckoutRefusal {
                code: CheckoutRefusalCode::NoPullRequest,
                message: "none".to_owned(),
            }),
        ))));
        assert_eq!(refused.request_id, 7);
        assert_eq!(refused.terminal_id, id::encode(&ResourceId::local(3)));
        assert_eq!(
            refused.reply,
            CheckoutReply::Failed {
                error: CheckoutError::NoPullRequest,
                message: "none".to_owned()
            }
        );
        let unanswered = result(response(CheckoutOutcome::Failed {
            failure: CheckoutFailure::Unanswered,
            message: "gone".to_owned(),
        }));
        assert!(matches!(
            unanswered.reply,
            CheckoutReply::Failed {
                error: CheckoutError::Unanswered,
                ..
            }
        ));
        let log = CheckoutAnswer::Log(CheckoutLog::default());
        assert_eq!(
            result(response(CheckoutOutcome::Answered(Box::new(log.clone())))).reply,
            CheckoutReply::Answer(Box::new(log))
        );
    }
}
