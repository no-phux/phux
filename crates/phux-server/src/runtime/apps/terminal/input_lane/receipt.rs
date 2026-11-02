//! The receipt an `APPLY_INPUT` caller awaits for its PTY verdict.
#![allow(
    clippy::redundant_pub_crate,
    reason = "the connection read loop and command handlers use these through the lane's re-export"
)]

use phux_protocol::ids::InputOperationId;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::oneshot;

use super::{SharedOperationCache, delivery_unknown};

/// Owned completion of an input command whose lane admission already ran.
#[derive(Debug)]
pub(crate) struct InputReceipt {
    state: ReceiptState,
}

#[derive(Debug)]
enum ReceiptState {
    Ready(Option<CommandResult>),
    Waiting {
        result: oneshot::Receiver<CommandResult>,
        failure: ReceiptFailure,
    },
}

#[derive(Debug)]
pub(super) enum ReceiptFailure {
    Internal(&'static str),
    DeliveryUnknown,
    DeliveryUnknownAndCache {
        cache: SharedOperationCache,
        operation_id: InputOperationId,
    },
}

impl Future for InputReceipt {
    type Output = CommandResult;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.state {
            ReceiptState::Ready(result) => {
                Poll::Ready(result.take().unwrap_or_else(|| CommandResult::Error {
                    code: ErrorCode::InternalError,
                    message: "input receipt polled after completion".to_owned(),
                }))
            }
            ReceiptState::Waiting { result, failure } => match Pin::new(result).poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Ok(result)) => Poll::Ready(result),
                Poll::Ready(Err(_)) => Poll::Ready(failure.result()),
            },
        }
    }
}

impl ReceiptFailure {
    fn result(&self) -> CommandResult {
        match self {
            Self::Internal(message) => CommandResult::Error {
                code: ErrorCode::InternalError,
                message: (*message).to_owned(),
            },
            Self::DeliveryUnknown => delivery_unknown(),
            Self::DeliveryUnknownAndCache {
                cache,
                operation_id,
            } => {
                let result = delivery_unknown();
                cache.set_final(*operation_id, &result);
                result
            }
        }
    }
}

pub(super) const fn ready_receipt(result: CommandResult) -> InputReceipt {
    InputReceipt {
        state: ReceiptState::Ready(Some(result)),
    }
}

pub(super) const fn waiting_receipt(
    result: oneshot::Receiver<CommandResult>,
    failure: ReceiptFailure,
) -> InputReceipt {
    InputReceipt {
        state: ReceiptState::Waiting { result, failure },
    }
}
