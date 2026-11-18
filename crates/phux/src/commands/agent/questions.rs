//! Public question routes share phux-client with in-process callers.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::native_agent::{ClientError, questions};
use phux_protocol::native_agent::NativeDisposition;
use phux_protocol::native_agent::questions::QuestionReceipt;

use crate::commands::json_err::{self, CliError, codes};
use crate::exit_codes::{EXIT_FAILURE, EXIT_PARTIAL_VIEW, EXIT_USAGE};

pub(super) fn current(target: &str, socket: Option<PathBuf>) -> ExitCode {
    let Some(target) = questions::target(target) else {
        return usage("expected an exact local @N or AUTHORITY/^RESOURCE");
    };
    let Ok(runtime) = crate::commands::cli_runtime() else {
        return ExitCode::FAILURE;
    };
    let socket = socket.unwrap_or_else(phux_config::socket::default_socket_path);
    match runtime.block_on(questions::current(&socket, target)) {
        Ok(document) => crate::output::json(&document),
        Err(error) => report_error(&error),
    }
}

pub(super) fn respond(request_json: &str, socket: Option<PathBuf>) -> ExitCode {
    let answer = match serde_json::from_str(request_json) {
        Ok(answer) => answer,
        Err(error) => return usage(&error.to_string()),
    };
    let Ok(runtime) = crate::commands::cli_runtime() else {
        return ExitCode::FAILURE;
    };
    let socket = socket.unwrap_or_else(phux_config::socket::default_socket_path);
    match runtime.block_on(questions::respond(&socket, answer)) {
        Ok(receipt) => report_receipt(&receipt),
        Err(error) => report_error(&error),
    }
}

pub(super) fn receipt(
    question_json: &str,
    operation_id: &str,
    socket: Option<PathBuf>,
) -> ExitCode {
    let question = match serde_json::from_str(question_json) {
        Ok(question) => question,
        Err(error) => return usage(&error.to_string()),
    };
    let operation_id = match operation_id.to_owned().try_into() {
        Ok(operation_id) => operation_id,
        Err(error) => return usage(error),
    };
    let Ok(runtime) = crate::commands::cli_runtime() else {
        return ExitCode::FAILURE;
    };
    let socket = socket.unwrap_or_else(phux_config::socket::default_socket_path);
    match runtime.block_on(questions::receipt(&socket, question, operation_id)) {
        Ok(receipt) => report_receipt(&receipt),
        Err(error) => report_error(&error),
    }
}

fn report_receipt(receipt: &QuestionReceipt) -> ExitCode {
    let status = crate::output::json(receipt);
    if status != ExitCode::SUCCESS {
        return status;
    }
    match receipt.disposition {
        NativeDisposition::Unknown => ExitCode::from(EXIT_PARTIAL_VIEW),
        NativeDisposition::Refused => ExitCode::from(EXIT_FAILURE),
        _ => status,
    }
}

fn usage(message: &str) -> ExitCode {
    json_err::emit(
        true,
        &CliError::new(codes::USAGE_ERROR, message, "No request was sent."),
        EXIT_USAGE,
    )
}

fn report_error(error: &ClientError) -> ExitCode {
    json_err::emit(
        true,
        &CliError::new(
            codes::AGENT_SESSION_REFUSED,
            error.to_string(),
            "Re-read the exact question or look up the original receipt; do not resend after an uncertain result.",
        ),
        EXIT_FAILURE,
    )
}
