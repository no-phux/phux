//! Read live questions and submit or recover a structured response.

use std::path::Path;
use std::time::Duration;

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::questions::{
    CurrentQuestions, QuestionOperationId, QuestionReceipt, QuestionRef, QuestionTarget,
    RespondQuestion,
};
use phux_protocol::native_agent::{NativeDisposition, NativeRequest, NativeRequestPayload};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode};
use serde::de::DeserializeOwned;

use super::ClientError;
use crate::attach::connection::{Connection, Dial};

const DEADLINE: Duration = Duration::from_secs(8);

/// Parse `@N` or `AUTHORITY/^RESOURCE`; never resolve focus or a moving name.
#[must_use]
pub fn target(value: &str) -> Option<QuestionTarget> {
    if let Some((authority, resource)) = value.split_once("/^") {
        return Some(QuestionTarget::Key {
            authority: authority.parse().ok()?,
            resource: resource.parse().ok()?,
        });
    }
    let id = value.strip_prefix('@')?.parse::<u32>().ok()?;
    (id != 0).then(|| QuestionTarget::Handle {
        resource: ResourceId::local(id),
    })
}

/// Observe all current questions on this exact local wire handle.
///
/// # Errors
/// Transport failure, refusal, or malformed server reply.
pub async fn current(
    socket: &Path,
    target: QuestionTarget,
) -> Result<CurrentQuestions, ClientError> {
    current_on(&Dial::uds(socket), target).await
}

/// Observe current questions through an explicit dial without changing origin.
///
/// # Errors
/// Transport failure, refusal, or malformed server reply.
pub async fn current_on(
    dial: &Dial,
    target: QuestionTarget,
) -> Result<CurrentQuestions, ClientError> {
    read(dial, NativeRequest::CurrentQuestions { target }).await
}

/// Observe current questions over an open connection that carries no other
/// subscription.
///
/// # Errors
/// Transport failure, refusal, or malformed server reply.
pub async fn current_over(
    connection: &mut Connection,
    target: QuestionTarget,
) -> Result<CurrentQuestions, ClientError> {
    require_current_questions(connection)?;
    exchange(
        connection,
        prepare(NativeRequest::CurrentQuestions { target })?,
    )
    .await
}

/// Submit the response once; never retry a potentially delivered mutation.
///
/// # Errors
/// Returns validation and pre-send connection failures, or a definite refusal.
/// A lost or unreadable submit reply is a receipt with `unknown` disposition.
pub async fn respond(
    socket: &Path,
    answer: RespondQuestion,
) -> Result<QuestionReceipt, ClientError> {
    respond_on(&Dial::uds(socket), answer).await
}

/// Submit once through an explicit dial and observe dispatch on that same connection.
///
/// # Errors
/// As with [`respond`]. Post-send uncertainty returns an unknown receipt.
pub async fn respond_on(
    dial: &Dial,
    answer: RespondQuestion,
) -> Result<QuestionReceipt, ClientError> {
    let operation_id = answer.operation_id;
    let lookup = prepare(NativeRequest::QuestionReceipt {
        question: answer.question.clone(),
        operation_id,
    })?;
    let request = prepare(NativeRequest::RespondQuestion { answer })?;
    let mut connection = connect(dial).await?;
    let result = exchange(&mut connection, request).await;
    match result {
        Ok(receipt) if receipt_id(&receipt, operation_id) => {
            if receipt.awaiting_dispatch == Some(true) {
                Ok(await_controller(&mut connection, lookup, operation_id).await)
            } else {
                Ok(receipt)
            }
        }
        Err(refusal @ ClientError::Refused { code, .. }) if definite_refusal(code) => Err(refusal),
        _ => Ok(unknown(operation_id)),
    }
}

async fn await_controller(
    connection: &mut Connection,
    lookup: NativeRequestPayload,
    operation_id: QuestionOperationId,
) -> QuestionReceipt {
    // The original connection's live authority is checked just before
    // handoff. Keep it open while reading the ledger; never resend a response.
    tokio::time::timeout(DEADLINE, async {
        loop {
            tokio::time::sleep(Duration::from_millis(75)).await;
            match exchange::<QuestionReceipt>(connection, lookup.clone()).await {
                Ok(receipt) if receipt_id(&receipt, operation_id) => {
                    if receipt.awaiting_dispatch == Some(false)
                        && receipt.disposition != NativeDisposition::Unknown
                    {
                        return receipt;
                    }
                }
                _ => return unknown(operation_id),
            }
        }
    })
    .await
    .unwrap_or_else(|_| unknown(operation_id))
}

fn unknown(operation_id: QuestionOperationId) -> QuestionReceipt {
    QuestionReceipt {
        schema_version: 1,
        operation_id,
        disposition: NativeDisposition::Unknown,
        awaiting_dispatch: None,
        detail: Some("answer outcome unknown; query its receipt and do not resend".into()),
    }
}

/// Retrieve a retained response receipt without resubmitting the intent.
///
/// # Errors
/// Transport failure, refusal, or malformed/mismatched server reply. These do
/// not establish that the original response failed.
pub async fn receipt(
    socket: &Path,
    question: QuestionRef,
    operation_id: QuestionOperationId,
) -> Result<QuestionReceipt, ClientError> {
    receipt_on(&Dial::uds(socket), question, operation_id).await
}

/// Read a retained receipt through the chosen origin, without mutating it.
///
/// # Errors
/// As with [`receipt`]. Lookup failure establishes nothing about the original response.
pub async fn receipt_on(
    dial: &Dial,
    question: QuestionRef,
    operation_id: QuestionOperationId,
) -> Result<QuestionReceipt, ClientError> {
    let receipt = read(
        dial,
        NativeRequest::QuestionReceipt {
            question,
            operation_id,
        },
    )
    .await?;
    if !receipt_id(&receipt, operation_id) {
        return Err(ClientError::Protocol("mismatched question receipt".into()));
    }
    Ok(receipt)
}

fn receipt_id(receipt: &QuestionReceipt, operation_id: QuestionOperationId) -> bool {
    receipt.operation_id == operation_id
}

const fn definite_refusal(code: ErrorCode) -> bool {
    matches!(
        code,
        ErrorCode::InvalidCommand
            | ErrorCode::PermissionDenied
            | ErrorCode::PreconditionFailed
            | ErrorCode::TerminalNotFound
            | ErrorCode::Overflow
            | ErrorCode::Unsupported
    )
}

async fn read<T: DeserializeOwned>(dial: &Dial, request: NativeRequest) -> Result<T, ClientError> {
    let request = prepare(request)?;
    let mut connection = connect(dial).await?;
    exchange(&mut connection, request).await
}

fn prepare(request: NativeRequest) -> Result<NativeRequestPayload, ClientError> {
    NativeRequestPayload::new(request).map_err(ClientError::Invalid)
}

async fn connect(dial: &Dial) -> Result<Connection, ClientError> {
    let connection = tokio::time::timeout(DEADLINE, Connection::connect_dial(dial))
        .await
        .map_err(|_| ClientError::Protocol("question connection timed out before send".into()))?
        .map_err(ClientError::Transport)?;
    require_current_questions(&connection)?;
    Ok(connection)
}

fn require_current_questions(connection: &Connection) -> Result<(), ClientError> {
    if connection
        .server_features_ext()
        .contains(ServerFeatureExt::CurrentQuestions)
    {
        return Ok(());
    }
    Err(ClientError::Refused {
        code: ErrorCode::Unsupported,
        message: "server does not advertise current questions".into(),
    })
}

async fn exchange<T: DeserializeOwned>(
    connection: &mut Connection,
    request: NativeRequestPayload,
) -> Result<T, ClientError> {
    let (result, _) = tokio::time::timeout(
        DEADLINE,
        connection.request(1, Command::NativeAgent { request }),
    )
    .await
    .map_err(|_| ClientError::Protocol("question request timed out".into()))?
    .map_err(ClientError::Transport)?
    .into_parts();
    match result {
        CommandResult::OkWith(CommandValue::Json(value)) => serde_json::from_str(&value)
            .map_err(|error| ClientError::Protocol(format!("invalid question reply: {error}"))),
        CommandResult::Error { code, message } => Err(ClientError::Refused { code, message }),
        _ => Err(ClientError::Protocol(
            "no question result in the reply".into(),
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;
    use crate::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::caps::ServerFeatureExtSet;
    use phux_protocol::wire::frame::FrameKind;
    use tokio::net::UnixListener;

    fn answer() -> RespondQuestion {
        RespondQuestion {
            operation_id: QuestionOperationId::new(u64::MAX).unwrap(),
            question: QuestionRef {
                authority: phux_protocol::ids::AuthorityId::new([1; 16]).unwrap(),
                conversation: phux_protocol::ids::ResourceKey::new([2; 16]).unwrap(),
                activation: "activation-A".into(),
                id: "callback-1".into(),
                revision: "1".repeat(64),
            },
            response: serde_json::json!({"option_id":"deny-once"}),
        }
    }

    async fn exchange_without_receipt(
        features: &[ServerFeatureExt],
    ) -> (Result<QuestionReceipt, ClientError>, Vec<FrameKind>) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("question.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let spec = ScriptSpec::new().server_features_ext(ServerFeatureExtSet::with(features));
        let task =
            tokio::spawn(
                async move { ScriptedServer::accept_then_drop_reply(&listener, spec).await },
            );
        let result = respond(&socket, answer()).await;
        (result, task.await.unwrap())
    }

    #[tokio::test]
    async fn lost_reply_is_unknown_and_does_not_resubmit() {
        let (result, seen) = exchange_without_receipt(&[
            ServerFeatureExt::NativeAgent,
            ServerFeatureExt::CurrentQuestions,
        ])
        .await;
        let result = result.unwrap();
        assert_eq!(result.operation_id.get(), u64::MAX);
        assert_eq!(result.disposition, NativeDisposition::Unknown);
        assert_eq!(result.awaiting_dispatch, None);
        let requests: Vec<_> = seen
            .iter()
            .filter_map(|frame| match frame {
                FrameKind::Command {
                    command: Command::NativeAgent { request },
                    ..
                } => Some(request.request()),
                _ => None,
            })
            .collect();
        assert_eq!(
            requests,
            vec![&NativeRequest::RespondQuestion { answer: answer() }]
        );
    }

    #[tokio::test]
    async fn an_older_native_peer_does_not_get_an_answer_request() {
        let (result, seen) = exchange_without_receipt(&[ServerFeatureExt::NativeAgent]).await;
        assert!(matches!(
            result,
            Err(ClientError::Refused {
                code: ErrorCode::Unsupported,
                ..
            })
        ));
        assert!(
            !seen
                .iter()
                .any(|frame| matches!(frame, FrameKind::Command { .. }))
        );
    }
}
