//! Exact SDK command ingress; never translates into terminal key atoms.
use crate::commands::json_err::{self, CliError, codes};
use crate::exit_codes::EXIT_USAGE;
use phux_client::attach::connection::Connection;
use phux_protocol::{
    caps::ServerFeatureExt,
    ids::ResourceId,
    native_agent::{
        NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent, NativeRequest,
        NativeRequestPayload,
    },
    wire::frame::{AgentEvent, Command, CommandResult, CommandValue, ErrorCode, FrameKind},
};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

pub(super) fn run(
    target: &str,
    activation: &str,
    operation_id: u64,
    intent_json: &str,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let parsed = target
        .strip_prefix('@')
        .and_then(|id| id.parse::<u32>().ok())
        .filter(|id| *id != 0);
    let Some(id) = parsed else {
        return json_err::emit(
            json,
            &CliError::new(
                codes::USAGE_ERROR,
                "native-command requires the exact local AgentSession @N",
                "Use the AgentSession ID and activation returned by native launch, not its Terminal or an inferred selector.",
            ),
            EXIT_USAGE,
        );
    };
    if operation_id == 0 || activation.is_empty() {
        return json_err::emit(
            json,
            &CliError::new(
                codes::USAGE_ERROR,
                "native-command requires a nonzero operation ID and exact activation",
                "Preserve the original operation identity; do not issue a replacement prompt after an uncertain reply.",
            ),
            EXIT_USAGE,
        );
    }
    let intent: NativeIntent = match serde_json::from_str(intent_json) {
        Ok(intent) => intent,
        Err(error) => {
            return json_err::emit(
                json,
                &CliError::new(
                    codes::USAGE_ERROR,
                    format!("invalid typed native intent: {error}"),
                    "Use a structured prompt, interrupt, or answer intent.",
                ),
                EXIT_USAGE,
            );
        }
    };
    if let Err(message) = intent.validate() {
        return json_err::emit(
            json,
            &CliError::new(
                codes::USAGE_ERROR,
                message,
                "Correct the typed intent; nothing was submitted.",
            ),
            EXIT_USAGE,
        );
    }
    let socket = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    let runtime = match crate::commands::cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    let command = NativeCommand {
        operation_id,
        activation: activation.to_owned(),
        intent,
    };
    let result = runtime.block_on(submit(&socket, id, command, json));
    report(result, target, activation, json)
}

async fn submit(
    socket: &Path,
    id: u32,
    command: NativeCommand,
    json: bool,
) -> Result<NativeCommandResult, ExitCode> {
    let operation_id = command.operation_id;
    let request = match NativeRequestPayload::new(NativeRequest::Command {
        agent_session_id: ResourceId::local(id),
        command,
    }) {
        Ok(request) => Command::NativeAgent { request },
        Err(message) => return Ok(NativeCommandResult {
            operation_id,
            disposition: NativeDisposition::Refused,
            detail: Some(message),
        }),
    };
    let mut connection = Connection::connect(socket).await
        .map_err(|error| json_err::report_no_server(json, &error, socket, "agent native-command"))?;
    let result = request_receipt(&mut connection, id, operation_id, request, json, socket).await;
    drop(connection);
    result
}

async fn request_receipt(
    connection: &mut Connection,
    id: u32,
    operation_id: u64,
    request: Command,
    json: bool,
    socket: &Path,
) -> Result<NativeCommandResult, ExitCode> {
    if !connection.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated.server_features_ext.contains(ServerFeatureExt::NativeAgent)
    }) {
        return Ok(NativeCommandResult {
            operation_id,
            disposition: NativeDisposition::Refused,
            detail: Some("The executing host does not support native SDK commands; upgrade phux. Nothing was submitted.".into()),
        });
    }
    connection.send(&FrameKind::SubscribeEvents { terminal: Some(ResourceId::local(id)), after_seq: Some(0) }).await
        .map_err(|error| json_err::report_no_server(json, &error, socket, "agent native-command"))?;
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
        CommandResult::OkWith(CommandValue::Json(value)) =>
            match serde_json::from_str::<NativeCommandResult>(&value) {
                Ok(receipt) if receipt.operation_id == operation_id => receipt,
                _ => uncertain(operation_id, "server returned an invalid or mismatched native receipt"),
            },
        CommandResult::Error { code, message } => NativeCommandResult {
            operation_id,
            disposition: if matches!(code, ErrorCode::InputDeliveryUnknown | ErrorCode::IncarnationChanged | ErrorCode::InternalError) {
                NativeDisposition::Unknown
            } else { NativeDisposition::Refused },
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
    }).await.map_or_else(
        |_| NativeCommandResult {
            operation_id,
            disposition: NativeDisposition::Unknown,
            detail: Some("controller receipt was not observed before timeout; retain this exact operation identity, do not issue a replacement prompt".into()),
        },
        |result| result,
    )
}

fn report(
    result: Result<NativeCommandResult, ExitCode>,
    target: &str,
    activation: &str,
    json: bool,
) -> ExitCode {
    match result {
        Ok(receipt) => {
            let accepted = matches!(
                receipt.disposition,
                NativeDisposition::Queued
                    | NativeDisposition::Admitted
                    | NativeDisposition::Completed
            );
            if json {
                let code = crate::output::json(
                    &serde_json::json!({"schema_version":1,"agent_session_id":target,"activation":activation,"result":receipt}),
                );
                if code != ExitCode::SUCCESS {
                    return code;
                }
            } else {
                match serde_json::to_string(&receipt) {
                    Ok(rendered) => outln!("{rendered}"),
                    Err(error) => {
                        error!("could not render native receipt: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            if accepted {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            }
        }
        Err(code) => code,
    }
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
