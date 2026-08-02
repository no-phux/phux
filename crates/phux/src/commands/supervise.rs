//! `phux take` / `phux give` / `phux signal` — the supervisory verbs
//! (ADR-0033, "take the wheel + kill").
//!
//! Each resolves a selector client-side to one pane (the same front door
//! `send-keys` / `run` use) and issues a single control command over a fresh
//! connection: `ACQUIRE_INPUT` (seize the input lease), `RELEASE_INPUT`, or
//! `SIGNAL_TERMINAL`.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_protocol::wire::frame::{
    Command as WireCommand, CommandResult, InputMode, TerminalSignal,
};
use phux_server::runtime::default_socket_path;

use crate::commands::{
    SignalArg, parse_selector, report_no_server, request_command, resolve_target,
};

/// `phux take TARGET` — seize the input lease over the resolved pane so only
/// this client's input reaches the PTY (ADR-0033). Uses `Seize` mode, so it
/// preempts any current holder.
pub(crate) fn run_take(target: &str, socket: Option<PathBuf>) -> ExitCode {
    run_lease(target, socket, true)
}

/// `phux give TARGET` — release the input lease over the resolved pane,
/// returning it to open input (ADR-0033). Idempotent.
pub(crate) fn run_give(target: &str, socket: Option<PathBuf>) -> ExitCode {
    run_lease(target, socket, false)
}

fn run_lease(target: &str, socket: Option<PathBuf>, take: bool) -> ExitCode {
    let verb = if take { "take" } else { "give" };
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let terminal_id = match resolve_target(&socket_path, &selector, verb).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let command = if take {
            WireCommand::AcquireInput {
                terminal_id,
                mode: InputMode::Seize,
                ttl_ms: 0,
            }
        } else {
            WireCommand::ReleaseInput { terminal_id }
        };
        match request_command(&socket_path, command).await {
            Ok(CommandResult::Ok) => {
                if take {
                    outln!("phux: took the wheel of {target}");
                } else {
                    outln!("phux: released the wheel of {target}");
                }
                ExitCode::SUCCESS
            }
            Ok(CommandResult::Error { message, .. }) => {
                eprintln!("phux: {verb} refused for {target}: {message}");
                ExitCode::from(2)
            }
            Ok(other) => {
                eprintln!(
                    "phux: {target}: {}",
                    phux_client::explain::explain_unexpected(verb, &other)
                );
                ExitCode::from(2)
            }
            Err(err) => report_no_server(&err, &socket_path, verb),
        }
    })
}

/// `phux signal TARGET SIGNAL` — deliver a POSIX signal to the resolved pane's
/// process group (ADR-0033). `freeze`/`resume` is the reversible brake.
pub(crate) fn run_signal(target: &str, signal: SignalArg, socket: Option<PathBuf>) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let wire_signal = TerminalSignal::from(signal);
    rt.block_on(async move {
        let terminal_id = match resolve_target(&socket_path, &selector, "signal").await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match request_command(
            &socket_path,
            WireCommand::SignalTerminal {
                terminal_id,
                signal: wire_signal,
            },
        )
        .await
        {
            Ok(CommandResult::Ok) => {
                outln!("phux: signalled {target} ({signal:?})");
                ExitCode::SUCCESS
            }
            Ok(CommandResult::Error { message, .. }) => {
                eprintln!("phux: signal refused for {target}: {message}");
                ExitCode::from(2)
            }
            Ok(other) => {
                eprintln!(
                    "phux: {target}: {}",
                    phux_client::explain::explain_unexpected("signal", &other)
                );
                ExitCode::from(2)
            }
            Err(err) => report_no_server(&err, &socket_path, "signal"),
        }
    })
}
