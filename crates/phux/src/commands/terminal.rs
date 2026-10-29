//! Thin JSON CLI adapter for the shared terminal workflow.
use crate::commands::{TerminalAction, cli_runtime};
use phux_protocol::api::TerminalCreate;
use std::{path::PathBuf, process::ExitCode};

pub(crate) fn run(action: &TerminalAction, socket: Option<PathBuf>) -> ExitCode {
    let socket = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    let runtime = match cli_runtime() {
        Ok(runtime) => runtime,
        Err(code) => return code,
    };
    match action {
        TerminalAction::Describe => {
            match runtime.block_on(phux_client::terminal::describe_on(&socket)) {
                Ok(document) => crate::output::json(&document),
                Err(error) => {
                    eprintln!(
                        "{}",
                        serde_json::json!({"schema_version":1,"error":{"code":error.code(),"message":error.to_string()}})
                    );
                    ExitCode::FAILURE
                }
            }
        }
        TerminalAction::Operation {
            session,
            idempotency_key,
        } => {
            let request = phux_protocol::api::TerminalOperation {
                session: session.clone(),
                idempotency_key: idempotency_key.clone(),
            };
            match runtime.block_on(phux_client::terminal::operation_on(&socket, &request)) {
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
                        serde_json::json!({"schema_version":1,"error":{"code":"invalid_argument","outcome":"not_sent","message":error.to_string()}})
                    );
                    return ExitCode::from(2);
                }
            };
            match runtime.block_on(phux_client::terminal::create_on(&socket, &request)) {
                Ok(result) => crate::output::json(&result),
                Err(error) => {
                    eprintln!("{}", error.document(&request.idempotency_key));
                    ExitCode::FAILURE
                }
            }
        }
    }
}
