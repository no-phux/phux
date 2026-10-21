//! Typed native launch, exact command ingress, and stored launch recipe reads.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::Path;

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{
    NativeCommand, NativeCommandResult, NativeDisposition, NativeRequest, NativeRequestPayload,
    SpawnNativeAgent, SpawnedNativeAgent,
};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ErrorCode, FrameKind, Scope,
};
use phux_protocol::wire::info::SessionSnapshot;

use super::ClientError;
use crate::attach::AttachError;
use crate::attach::connection::{Answer, Connection};
use crate::state::Degradation;

/// Spawn a native agent once; transport loss never triggers a second launch.
///
/// # Errors
/// Returns local validation, transport, broker refusal, or malformed reply errors.
/// Delivery may be unknown on transport loss: do not use the controller-only
/// `ClientError::is_retryable` classification to replay a spawn.
pub async fn spawn(
    socket: &Path,
    spec: SpawnNativeAgent,
) -> Result<SpawnedNativeAgent, ClientError> {
    let request =
        NativeRequestPayload::new(NativeRequest::Spawn { spec }).map_err(ClientError::Invalid)?;
    let mut connection = Connection::connect(socket)
        .await
        .map_err(ClientError::Transport)?;
    // Connection::request checks NativeAgent negotiation before sending the tag.
    let (result, _) = connection
        .request(1, Command::NativeAgent { request })
        .await
        .map_err(ClientError::Transport)?
        .into_parts();
    drop(connection);
    match result {
        CommandResult::OkWith(CommandValue::Json(value)) => serde_json::from_str(&value)
            .map_err(|error| ClientError::Protocol(format!("invalid native spawn reply: {error}"))),
        CommandResult::Error { code, message } => Err(ClientError::Refused { code, message }),
        _ => Err(ClientError::Protocol(
            "server returned no native spawn transaction result".to_owned(),
        )),
    }
}

/// Read local terminals' native recipes, retaining inline partial-fleet notices.
/// The caller owns portability policy (stripping environment and source bindings).
///
/// # Errors
/// Returns transport, metadata refusal, or invalid stored recipe errors. Notices
/// are delivered before decoding/refusal, even if a later recipe read fails.
pub async fn fetch_launch_recipes(
    socket: &Path,
    snapshot: &SessionSnapshot,
    mut on_degradation: impl FnMut(&Degradation),
) -> Result<HashMap<ResourceId, SpawnNativeAgent>, ClientError> {
    let mut connection = Connection::connect(socket)
        .await
        .map_err(ClientError::Transport)?;
    let mut records = HashMap::new();
    for (index, resource) in crate::resource::terminals(snapshot)
        .filter(|resource| resource.id.is_local())
        .enumerate()
    {
        let request_id = u32::try_from(index)
            .map_err(|_| ClientError::Invalid("too many native archive resources".into()))?
            .saturating_add(1);
        let (answer, interleaved) = connection
            .request_metadata(
                request_id,
                Scope::Resource(resource.id.clone()),
                "phux.native-launch/v1".to_owned(),
            )
            .await
            .map_err(ClientError::Transport)?
            .into_parts();
        on_degradation(&Degradation::from_interleaved(&interleaved));
        if let Some(spec) = launch_recipe(answer, &resource.id)? {
            records.insert(resource.id.clone(), spec);
        }
    }
    drop(connection);
    Ok(records)
}

fn launch_recipe(
    answer: Answer<Option<Vec<u8>>>,
    resource: &ResourceId,
) -> Result<Option<SpawnNativeAgent>, ClientError> {
    match answer {
        Answer::Ok(Some(bytes)) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            ClientError::Protocol(format!("invalid native SDK recipe for {resource}: {error}"))
        }),
        Answer::Ok(None) => Ok(None),
        Answer::Err(refusal) => Err(ClientError::Protocol(format!(
            "could not capture native SDK recipe for {resource}: {refusal}"
        ))),
    }
}

/// Submit one exact operation and observe its controller receipt. Never retries input.
///
/// # Errors
/// Connect/subscription failures occur before the command is sent. Once delivery
/// is possible, loss or mismatched replies yield an Unknown receipt, not a retry.
pub async fn submit_command(
    socket: &Path,
    id: u32,
    command: NativeCommand,
) -> Result<NativeCommandResult, AttachError> {
    let operation_id = command.operation_id;
    let request = match NativeRequestPayload::new(NativeRequest::Command {
        agent_session_id: ResourceId::local(id),
        command,
    }) {
        Ok(request) => Command::NativeAgent { request },
        Err(message) => {
            return Ok(NativeCommandResult {
                operation_id,
                disposition: NativeDisposition::Refused,
                detail: Some(message),
            });
        }
    };
    let mut connection = Connection::connect(socket).await?;
    let result = request_receipt(&mut connection, id, operation_id, request).await;
    drop(connection);
    result
}

async fn request_receipt(
    connection: &mut Connection,
    id: u32,
    operation_id: u64,
    request: Command,
) -> Result<NativeCommandResult, AttachError> {
    if !connection.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features_ext
            .contains(ServerFeatureExt::NativeAgent)
    }) {
        return Ok(NativeCommandResult {
            operation_id,
            disposition: NativeDisposition::Refused,
            detail: Some("The executing host does not support native SDK commands; upgrade phux. Nothing was submitted.".into()),
        });
    }
    connection
        .send(&FrameKind::SubscribeEvents {
            terminal: Some(ResourceId::local(id)),
            after_seq: Some(0),
        })
        .await?;
    let (reply, frames) = match connection.request(1, request).await {
        Ok(reply) => reply.into_parts(),
        Err(error) => return Ok(uncertain(operation_id, &error.to_string())),
    };
    let receipt = command_receipt(reply, operation_id);
    if receipt.disposition != NativeDisposition::Queued {
        return Ok(receipt);
    }
    for frame in frames {
        if let Some(result) = native_result(frame, id, operation_id) {
            return Ok(result);
        }
    }
    Ok(await_receipt(connection, id, operation_id).await)
}

fn command_receipt(reply: CommandResult, operation_id: u64) -> NativeCommandResult {
    match reply {
        CommandResult::OkWith(CommandValue::Json(value)) => {
            match serde_json::from_str::<NativeCommandResult>(&value) {
                Ok(receipt) if receipt.operation_id == operation_id => receipt,
                _ => uncertain(
                    operation_id,
                    "server returned an invalid or mismatched native receipt",
                ),
            }
        }
        CommandResult::Error { code, message } => NativeCommandResult {
            operation_id,
            // `UNSUPPORTED` promises nothing ran, so it is refused like any
            // other definite refusal. A code this build does not know, the
            // client's own `UNREADABLE_REPLY` included, may mean the prompt was
            // delivered: unknown, so the caller does not re-prompt.
            disposition: if matches!(
                code,
                ErrorCode::InputDeliveryUnknown
                    | ErrorCode::IncarnationChanged
                    | ErrorCode::InternalError
                    | ErrorCode::Unknown(_)
            ) {
                NativeDisposition::Unknown
            } else {
                NativeDisposition::Refused
            },
            detail: Some(message),
        },
        _ => uncertain(operation_id, "server did not return a native receipt"),
    }
}

async fn await_receipt(
    connection: &mut Connection,
    id: u32,
    operation_id: u64,
) -> NativeCommandResult {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let frame = match connection.recv().await {
                Ok(frame) => frame,
                Err(error) => return uncertain(operation_id, &error.to_string()),
            };
            if let Some(result) = native_result(frame, id, operation_id) {
                return result;
            }
        }
    }).await.unwrap_or_else(|_| NativeCommandResult {
        operation_id,
        disposition: NativeDisposition::Unknown,
        detail: Some("controller receipt was not observed before timeout; retain this exact operation identity, do not issue a replacement prompt".into()),
    })
}

fn native_result(frame: FrameKind, id: u32, operation: u64) -> Option<NativeCommandResult> {
    match frame {
        FrameKind::Event {
            event:
                AgentEvent::NativeAgentResult {
                    agent_session_id,
                    operation_id,
                    disposition,
                    detail,
                },
            ..
        } if agent_session_id == ResourceId::local(id) && operation_id == operation => {
            Some(NativeCommandResult {
                operation_id,
                disposition,
                detail,
            })
        }
        _ => None,
    }
}

fn uncertain(operation_id: u64, reason: &str) -> NativeCommandResult {
    NativeCommandResult {
        operation_id,
        disposition: NativeDisposition::Unknown,
        detail: Some(format!(
            "controller receipt unavailable: {reason}; retain the exact operation identity, do not send a replacement intent"
        )),
    }
}
