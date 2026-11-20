//! Thin JSON CLI adapter for the shared terminal workflow.
use crate::commands::{TerminalAction, cli_runtime};
use phux_protocol::api::TerminalCreate;
use std::{path::PathBuf, process::ExitCode};

mod run_command;

pub(crate) fn run(args: crate::commands::TerminalArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::TerminalArgs { action } = args;
    let action = &action;
    let local_socket = || {
        socket
            .clone()
            .unwrap_or_else(phux_config::socket::default_socket_path)
    };
    let runtime = match cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match action {
        TerminalAction::Run(args) => run_command::run(&runtime, socket, args),
        TerminalAction::Operation {
            session,
            idempotency_key,
            remote,
        } => {
            let request = phux_protocol::api::TerminalOperation {
                session: session.clone(),
                idempotency_key: idempotency_key.clone(),
            };
            let target = super::server_target::ServerSpec {
                socket,
                remote: remote.clone(),
            };
            let target = match target.resolve(&runtime, "terminal operation", true) {
                Ok(target) => target,
                Err(code) => return code,
            };
            match runtime.block_on(async {
                use phux_client::terminal::CreateError;
                request.command().map_err(CreateError::Invalid)?;
                let mut conn = target
                    .connect()
                    .await
                    .map_err(|err| CreateError::Lookup(phux_client::org::OrgError::Attach(err)))?;
                phux_client::terminal::operation(&mut conn, &request).await
            }) {
                Ok(result) => crate::output::json(&result),
                Err(error) => {
                    eprintln!("{}", error.document(idempotency_key));
                    ExitCode::FAILURE
                }
            }
        }
        TerminalAction::Create { request_json } => {
            let request: TerminalCreate = match serde_json::from_str(request_json) {
                Ok(request) => request,
                Err(error) => {
                    eprintln!(
                        "{}",
                        serde_json::to_value(phux_protocol::api::ErrorDocument {
                            schema_version: 1,
                            error: phux_protocol::api::OperationError {
                                code: phux_protocol::api::ErrorCode::InvalidArgument,
                                outcome: phux_protocol::api::OperationOutcome::NotSent,
                                message: error.to_string(),
                                idempotency_key: None,
                            },
                        })
                        .unwrap_or_default()
                    );
                    return ExitCode::from(2);
                }
            };
            match runtime.block_on(phux_client::terminal::create_on(&local_socket(), &request)) {
                Ok(result) => crate::output::json(&result),
                Err(error) => {
                    eprintln!("{}", error.document(&request.idempotency_key));
                    ExitCode::FAILURE
                }
            }
        }
    }
}
