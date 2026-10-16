//! Exact SDK command ingress; never translates into terminal key atoms.
use crate::commands::json_err::{self, CliError, codes};
use crate::exit_codes::EXIT_USAGE;
use phux_protocol::native_agent::{
    NativeCommand, NativeCommandResult, NativeDisposition, NativeIntent,
};
use std::{path::PathBuf, process::ExitCode};

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
    let result = runtime
        .block_on(phux_client::native_agent::submit_command(
            &socket, id, command,
        ))
        .map_err(|error| json_err::report_no_server(json, &error, &socket, "agent native-command"));
    report(result, target, activation, json)
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
