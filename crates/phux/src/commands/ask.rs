use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::ask::AskedPayload;
use phux_client::attach::AttachError;
use phux_client::selector::format_terminal_id;
use phux_protocol::TerminalId;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

pub(crate) fn run_ask(
    target: &str,
    id: String,
    suggestions: Vec<String>,
    elapsed_seconds: Option<u64>,
    json: bool,
    question: String,
    socket: Option<PathBuf>,
) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let pane = match resolve_target(&socket_path, &selector, "ask", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let payload = AskedPayload {
            id,
            question,
            suggestions,
            elapsed_seconds,
        };
        match phux_client::ask::report(&socket_path, pane.clone(), payload.clone()).await {
            Ok(()) => print_success(&pane, &payload, json),
            Err(err @ AttachError::Io(_)) => {
                json_err::report_no_server(json, &err, &socket_path, "ask")
            }
            Err(AttachError::Refused(msg)) => {
                eprintln!("phux: cannot report ask for '{target}': {msg}");
                ExitCode::FAILURE
            }
            Err(err) => {
                eprintln!("phux: ask failed: {err}");
                ExitCode::FAILURE
            }
        }
    })
}

fn print_success(pane: &TerminalId, payload: &AskedPayload, json: bool) -> ExitCode {
    if json {
        match serde_json::to_string_pretty(&success_json(pane, payload)) {
            Ok(line) => outln!("{line}"),
            Err(err) => {
                eprintln!("phux: failed to serialize ask: {err}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        outln!("{}", success_text(pane, payload));
    }
    ExitCode::SUCCESS
}

fn success_json(pane: &TerminalId, payload: &AskedPayload) -> serde_json::Value {
    serde_json::json!({
        "event": "asked",
        "terminal": format_terminal_id(pane),
        "id": payload.id,
        "question": payload.question,
        "suggestions": payload.suggestions,
        "elapsed_seconds": payload.elapsed_seconds,
    })
}

fn success_text(pane: &TerminalId, payload: &AskedPayload) -> String {
    format!(
        "reported ask {} to {}",
        payload.id,
        format_terminal_id(pane)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn satellite_success_outputs_canonical_selector() {
        let pane = TerminalId::satellite("region/@build", 7);
        let payload = AskedPayload {
            id: "q1".to_owned(),
            question: "Continue?".to_owned(),
            suggestions: vec!["yes".to_owned()],
            elapsed_seconds: Some(5),
        };
        assert_eq!(
            success_json(&pane, &payload)["terminal"],
            "region/@build/@7"
        );
        assert_eq!(
            success_text(&pane, &payload),
            "reported ask q1 to region/@build/@7"
        );
    }
}
